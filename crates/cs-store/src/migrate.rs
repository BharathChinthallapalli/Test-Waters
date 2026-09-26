//! Forward-only migrations tracked with `PRAGMA user_version` (R3.2, R3.3).
//!
//! Owned by unit `schema`: schema version 1 from the design, including the
//! append-only triggers; refusal to open a newer schema; the owner-only
//! `VACUUM INTO backup-v<from>.db` before migrating an existing database; and the
//! function that deletes those backups once a migrated startup has verified.
//!
//! Migration `n` (1-based) is `MIGRATIONS[n - 1]`. Each one runs in its own
//! `BEGIN IMMEDIATE` transaction that also sets `user_version = n`, so a failed
//! migration leaves the database at the last version that fully applied.
//! `foreign_keys` is on and can't be switched inside a transaction, so a future
//! migration that rebuilds a referenced table (`runs`, `events`) has to handle
//! that when it is written.
//!
//! The backup is written to `backup-v<from>-next.db`, created empty and owner-only
//! through [`crate::fsperm`], and renamed to `backup-v<from>.db` once complete, so
//! a backup left by an earlier failed attempt survives until a new one exists.
//! Both names match `backup-v*.db`, which [`remove_migration_backups`] deletes.
//! `VACUUM INTO` fills the empty file: SQLite 3.53.2's `sqlite3RunVacuum` (bundled
//! `sqlite3.c`) accepts an existing target as long as it is empty ("output file
//! already exists" only when its size is above zero), and it uses the main
//! database's pager flags, so the backup is written with `synchronous=FULL`.
//! A journal SQLite may create beside it takes the backup's mode
//! (`findCreateFileMode`). `VACUUM INTO` copies live rows only, so the backup
//! carries no free-page residue.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};

use crate::db::sqlite_literal_path;
use crate::fsperm;

/// Schema version 1, exactly as in the feature 02 design ("Schema version 1").
const SCHEMA_V1: &str = r"
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE runs (run_id TEXT PRIMARY KEY, created_ms INTEGER NOT NULL,
                   last_seq INTEGER NOT NULL, last_hash TEXT NOT NULL);
CREATE TABLE events (
  global_pos INTEGER PRIMARY KEY,            -- 1, 2, 3 … assigned by the writer, no gaps
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  seq INTEGER NOT NULL,                      -- 1, 2, 3 … per run
  kind TEXT NOT NULL,
  ts_ms INTEGER NOT NULL,                    -- Unix milliseconds (< 2^53, safe as a JSON number)
  body TEXT NOT NULL,                        -- canonical JSON, no floats, content only by address
  prev_hash TEXT NOT NULL,
  event_hash TEXT NOT NULL,
  UNIQUE (run_id, seq));
CREATE TRIGGER events_no_update BEFORE UPDATE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TABLE blobs (address TEXT PRIMARY KEY, bytes BLOB NOT NULL);
CREATE TABLE event_content (global_pos INTEGER NOT NULL REFERENCES events(global_pos),
                            address TEXT NOT NULL, PRIMARY KEY (global_pos, address));
";

/// Every migration in order. Only ever append; never edit a released entry.
const MIGRATIONS: &[&str] = &[SCHEMA_V1];

/// The schema version this build migrates to and understands.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

const _: () = assert!(MIGRATIONS.len() == CURRENT_SCHEMA_VERSION as usize);

/// The database's `user_version` is one this build doesn't know: newer than
/// [`CURRENT_SCHEMA_VERSION`], or negative, which Callsheet never writes (R3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedSchema {
    pub found: i64,
    pub supported: u32,
}

impl fmt::Display for UnsupportedSchema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "database schema version {} is not one this daemon understands (it knows up to {}); \
             refusing to modify the database",
            self.found, self.supported
        )
    }
}

impl std::error::Error for UnsupportedSchema {}

/// Why migrating failed.
#[derive(Debug)]
pub enum MigrateError {
    /// The schema is newer than this build knows; nothing was changed.
    UnsupportedSchema(UnsupportedSchema),
    /// Reading the schema version or contents failed; nothing was changed.
    Sqlite(rusqlite::Error),
    /// Replacing or creating the backup file failed; nothing was migrated.
    BackupFile { path: PathBuf, source: io::Error },
    /// `VACUUM INTO` the backup failed; nothing was migrated.
    Backup {
        path: PathBuf,
        source: rusqlite::Error,
    },
    /// Migration `version` failed and was rolled back; the database stays at
    /// `version - 1` and the backup, if one was taken, is kept.
    Migration {
        version: u32,
        source: rusqlite::Error,
    },
}

impl fmt::Display for MigrateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema(inner) => inner.fmt(f),
            Self::Sqlite(source) => write!(f, "cannot read the database schema: {source}"),
            Self::BackupFile { path, source } => {
                write!(f, "cannot create backup {}: {source}", path.display())
            }
            Self::Backup { path, source } => {
                write!(
                    f,
                    "cannot back up the database to {}: {source}",
                    path.display()
                )
            }
            Self::Migration { version, source } => {
                write!(f, "migration to schema version {version} failed: {source}")
            }
        }
    }
}

/// The message already includes the underlying error, so `source` is left empty
/// and a printed error chain says it once.
impl std::error::Error for MigrateError {}

/// What [`migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrated {
    /// Schema version before migrating.
    pub from: u32,
    /// Schema version now.
    pub to: u32,
    /// The backup taken first, if the database already held a schema. Delete it
    /// with [`remove_migration_backups`] once the startup has verified.
    pub backup: Option<PathBuf>,
}

/// Reads `PRAGMA user_version`.
pub fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// Accepts a schema version this build understands, from 0 (empty) to
/// [`CURRENT_SCHEMA_VERSION`].
pub fn ensure_supported(found: i64) -> Result<u32, UnsupportedSchema> {
    supported_up_to(found, CURRENT_SCHEMA_VERSION)
}

/// Brings the writer connection's database to [`CURRENT_SCHEMA_VERSION`].
///
/// A fresh database (version 0 and no schema) is migrated directly. An existing
/// one with pending migrations is first copied to `<data dir>/backup-v<from>.db`.
/// Running it again on an up-to-date database changes nothing (R3.2).
pub fn migrate(conn: &mut Connection, data_dir: &Path) -> Result<Migrated, MigrateError> {
    migrate_with(conn, data_dir, MIGRATIONS)
}

/// `<data dir>/backup-v<from>.db`.
pub fn backup_path(data_dir: &Path, from: u32) -> PathBuf {
    data_dir.join(format!("backup-v{from}.db"))
}

/// Deletes every regular file in `data_dir` whose name matches `backup-v*.db`,
/// and returns how many it deleted. Anything else, including directories,
/// symbolic links and names such as `backup-v1.db-journal`, is left alone.
///
/// It tries every match even after a failure, then reports the first failure,
/// so one stuck file doesn't keep the others. Called after a migrated startup
/// has passed `integrity_check` and a full verify, and during erasure.
pub fn remove_migration_backups(data_dir: &Path) -> io::Result<u64> {
    let mut removed = 0;
    let mut first_error = None;
    for entry in fs::read_dir(data_dir)? {
        let outcome = entry.and_then(|entry| {
            if is_backup_name(&entry.file_name()) && entry.file_type()?.is_file() {
                fs::remove_file(entry.path()).map(|()| true)
            } else {
                Ok(false)
            }
        });
        match outcome {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(removed), Err)
}

fn is_backup_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.len() >= "backup-v.db".len() && name.starts_with("backup-v") && name.ends_with(".db")
    })
}

fn supported_up_to(found: i64, supported: u32) -> Result<u32, UnsupportedSchema> {
    u32::try_from(found)
        .ok()
        .filter(|version| *version <= supported)
        .ok_or(UnsupportedSchema { found, supported })
}

/// [`migrate`] over an explicit migration list, so tests can add a version.
fn migrate_with(
    conn: &mut Connection,
    data_dir: &Path,
    migrations: &[&str],
) -> Result<Migrated, MigrateError> {
    let target = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
    let found = schema_version(conn).map_err(MigrateError::Sqlite)?;
    let from = supported_up_to(found, target).map_err(MigrateError::UnsupportedSchema)?;
    if from == target {
        return Ok(Migrated {
            from,
            to: from,
            backup: None,
        });
    }

    let backup = if is_fresh(conn, from)? {
        None
    } else {
        Some(back_up(conn, data_dir, from)?)
    };
    for (version, sql) in (1..=target).zip(migrations) {
        if version > from {
            apply(conn, version, sql)
                .map_err(|source| MigrateError::Migration { version, source })?;
        }
    }
    Ok(Migrated {
        from,
        to: target,
        backup,
    })
}

/// Version 0 with nothing in `sqlite_schema`: a database created just now.
fn is_fresh(conn: &Connection, version: u32) -> Result<bool, MigrateError> {
    if version != 0 {
        return Ok(false);
    }
    let objects: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
        .map_err(MigrateError::Sqlite)?;
    Ok(objects == 0)
}

/// Writes `backup-v<from>.db` through `backup-v<from>-next.db`. A backup of the
/// same version left by an earlier failed attempt is replaced only once the new
/// one is complete: the database is still at `from`, so the new copy is at least
/// as recent.
fn back_up(conn: &Connection, data_dir: &Path, from: u32) -> Result<PathBuf, MigrateError> {
    let path = backup_path(data_dir, from);
    let next = data_dir.join(format!("backup-v{from}-next.db"));
    let file_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| MigrateError::BackupFile { path, source }
    };
    // VACUUM INTO takes its file name as SQL text (`sqlite3RunVacuum`).
    let literal = sqlite_literal_path(&next);
    let target = literal.to_str().ok_or_else(|| {
        file_error(&next)(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backup path is not valid UTF-8",
        ))
    })?;
    remove_if_present(&next).map_err(file_error(&next))?;
    fsperm::create_new_owner_only(&next).map_err(file_error(&next))?;
    if let Err(source) = conn.execute("VACUUM INTO ?1", [target]) {
        // Best effort: a partial copy is of no use, and the database is intact.
        let _ = fs::remove_file(&next);
        return Err(MigrateError::Backup { path: next, source });
    }
    fs::rename(&next, &path).map_err(file_error(&path))?;
    Ok(path)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn apply(conn: &mut Connection, version: u32, sql: &str) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(sql)?;
    tx.pragma_update(None, "user_version", version)?;
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    const EXTRA: &str = "CREATE TABLE extra (x INTEGER);";

    fn open(dir: &Path) -> Connection {
        db::open_writer(dir).unwrap()
    }

    fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn insert_event(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO runs VALUES ('r1', 1, 1, 'h1');
             INSERT INTO events VALUES (1, 'r1', 1, 'run.started', 1, '{}', 'p', 'h1');",
        )
        .unwrap();
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn a_fresh_database_gets_schema_1_without_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open(dir.path());

        let migrated = migrate(&mut conn, dir.path()).unwrap();

        assert_eq!(
            migrated,
            Migrated {
                from: 0,
                to: CURRENT_SCHEMA_VERSION,
                backup: None
            }
        );
        assert_eq!(schema_version(&conn).unwrap(), 1);
        let names: Vec<_> = schema(&conn)
            .into_iter()
            .filter(|(kind, _, _)| kind != "index")
            .map(|(kind, name, _)| format!("{kind} {name}"))
            .collect();
        assert_eq!(
            names,
            [
                "table blobs",
                "table event_content",
                "table events",
                "table runs",
                "table settings",
                "trigger events_no_delete",
                "trigger events_no_update",
            ]
        );
        assert_eq!(remove_migration_backups(dir.path()).unwrap(), 0);
    }

    #[test]
    fn migrating_again_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open(dir.path());
        migrate(&mut conn, dir.path()).unwrap();
        let before = schema(&conn);
        drop(conn);

        let mut conn = open(dir.path());
        let migrated = migrate(&mut conn, dir.path()).unwrap();

        assert_eq!(migrated.from, migrated.to);
        assert_eq!(migrated.backup, None);
        assert_eq!(schema_version(&conn).unwrap(), 1);
        assert_eq!(schema(&conn), before);
    }

    #[test]
    fn a_newer_schema_is_refused_and_the_file_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(db::DATABASE_FILE_NAME);
        let mut conn = open(dir.path());
        migrate(&mut conn, dir.path()).unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
        // Checkpoint so the whole database is in the main file.
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .unwrap();
        let before = fs::read(&path).unwrap();

        let error = migrate(&mut conn, dir.path()).unwrap_err();

        assert!(matches!(
            error,
            MigrateError::UnsupportedSchema(UnsupportedSchema {
                found: 7,
                supported: 1
            })
        ));
        drop(conn);
        assert!(fs::read(&path).unwrap() == before, "database file changed");
        assert_eq!(remove_migration_backups(dir.path()).unwrap(), 0);
    }

    #[test]
    fn a_negative_schema_version_is_refused() {
        assert_eq!(
            ensure_supported(-1),
            Err(UnsupportedSchema {
                found: -1,
                supported: CURRENT_SCHEMA_VERSION
            })
        );
        assert_eq!(ensure_supported(0), Ok(0));
        assert_eq!(ensure_supported(1), Ok(1));
    }

    #[test]
    fn triggers_block_update_and_delete_on_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open(dir.path());
        migrate(&mut conn, dir.path()).unwrap();
        insert_event(&conn);

        for sql in [
            "UPDATE events SET kind = 'edited' WHERE global_pos = 1",
            "DELETE FROM events WHERE global_pos = 1",
        ] {
            let error = conn.execute(sql, []).unwrap_err();
            assert!(
                error.to_string().contains("events are append-only"),
                "{sql}: {error}"
            );
        }
        let kind: String = conn
            .query_row("SELECT kind FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kind, "run.started");
    }

    #[test]
    fn a_pending_migration_backs_up_the_existing_database_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open(dir.path());
        migrate_with(&mut conn, dir.path(), &[SCHEMA_V1]).unwrap();
        insert_event(&conn);

        let migrated = migrate_with(&mut conn, dir.path(), &[SCHEMA_V1, EXTRA]).unwrap();

        let backup = backup_path(dir.path(), 1);
        assert_eq!(
            migrated,
            Migrated {
                from: 1,
                to: 2,
                backup: Some(backup.clone())
            }
        );
        assert_eq!(schema_version(&conn).unwrap(), 2);
        assert!(!dir.path().join("backup-v1-next.db").exists());
        let copy = Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        assert_eq!(schema_version(&copy).unwrap(), 1);
        assert_eq!(count(&copy, "events"), 1);
        assert!(copy.prepare("SELECT * FROM extra").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&backup).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_stale_backup_of_the_same_version_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open(dir.path());
        migrate_with(&mut conn, dir.path(), &[SCHEMA_V1]).unwrap();
        insert_event(&conn);
        fs::write(backup_path(dir.path(), 1), b"left by an earlier attempt").unwrap();

        migrate_with(&mut conn, dir.path(), &[SCHEMA_V1, EXTRA]).unwrap();

        let copy = Connection::open(backup_path(dir.path(), 1)).unwrap();
        assert_eq!(count(&copy, "events"), 1);
    }

    #[test]
    fn a_failed_migration_rolls_back_and_keeps_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open(dir.path());
        migrate_with(&mut conn, dir.path(), &[SCHEMA_V1]).unwrap();
        let broken = "CREATE TABLE half (x INTEGER); CREATE TABLE settings (k TEXT);";

        let error = migrate_with(&mut conn, dir.path(), &[SCHEMA_V1, broken]).unwrap_err();

        assert!(matches!(error, MigrateError::Migration { version: 2, .. }));
        assert_eq!(schema_version(&conn).unwrap(), 1);
        assert!(conn.prepare("SELECT * FROM half").is_err());
        assert!(backup_path(dir.path(), 1).exists());
    }

    #[test]
    fn only_backup_files_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let keep = [
            "callsheet.db",
            "callsheet.db-wal",
            "backup-v1.db-journal",
            "backup-v1.dbx",
            "xbackup-v1.db",
            "backup-1.db",
        ];
        let remove = [
            "backup-v1.db",
            "backup-v12.db",
            "backup-v2-next.db",
            "backup-v.db",
        ];
        for name in keep.iter().chain(&remove) {
            fs::write(dir.path().join(name), b"").unwrap();
        }
        fs::create_dir(dir.path().join("backup-v3.db")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            dir.path().join("callsheet.db"),
            dir.path().join("backup-v4.db"),
        )
        .unwrap();

        assert_eq!(remove_migration_backups(dir.path()).unwrap(), 4);

        for name in remove {
            assert!(!dir.path().join(name).exists(), "{name}");
        }
        assert!(dir.path().join("backup-v3.db").is_dir());
        #[cfg(unix)]
        assert!(dir.path().join("backup-v4.db").is_symlink());
        for name in keep {
            assert!(dir.path().join(name).exists(), "{name}");
        }
    }
}
