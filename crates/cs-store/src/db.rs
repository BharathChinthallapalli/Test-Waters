//! Opening the database (feature 02 design "Store").
//!
//! Owned by unit `schema`: the writer connection and the read-only connection, each
//! with `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON` and
//! `secure_delete=ON`. The database file is created owner-only through
//! [`crate::fsperm`] before SQLite opens it; SQLite gives its `-wal` and `-shm`
//! files the same permissions as the database file.
//!
//! How that holds, in the SQLite 3.53.2 source bundled by `libsqlite3-sys` 0.38.2
//! (`sqlite3/sqlite3.c`):
//! - `findCreateFileMode` gives a file opened with `SQLITE_OPEN_WAL` or
//!   `SQLITE_OPEN_MAIN_JOURNAL` the mode, uid and gid of its database file.
//! - `unixOpenSharedMemory` creates `-shm` with `sStat.st_mode & 0777` of the
//!   database file.
//! - The main file itself would get `SQLITE_DEFAULT_FILE_PERMISSIONS` minus the
//!   umask, which is why we create it first and never pass `SQLITE_OPEN_CREATE`.
//!
//! An existing database file is opened as it is; its mode was set when it was
//! created, and the data directory around it is owner-only as well.
//!
//! Order at startup: [`open_writer`] (then `crate::migrate::migrate`), then
//! [`open_reader`]. The reader needs the database to be in WAL mode already: a
//! read-only connection can't change the journal mode, and in WAL mode it still
//! opens `-shm` read-write (`unixOpenSharedMemory`), which works because the daemon
//! owns the data directory.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, MAIN_DB, OpenFlags};

use crate::fsperm;
use crate::migrate::{self, UnsupportedSchema};

/// The database file's name inside the data directory.
pub const DATABASE_FILE_NAME: &str = "callsheet.db";

/// Why the database could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// Creating the empty owner-only database file failed.
    CreateFile { path: PathBuf, source: io::Error },
    /// SQLite could not open or configure the database.
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },
    /// The file could be opened for reading only, so it can't be the writer.
    WriterIsReadOnly { path: PathBuf },
    /// A pragma read back a different value from the one just set.
    PragmaNotApplied {
        pragma: &'static str,
        expected: &'static str,
        found: String,
    },
    /// The schema is newer than this build knows; the file was left untouched
    /// (R3.3).
    UnsupportedSchema(UnsupportedSchema),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateFile { path, source } => {
                write!(f, "cannot create {}: {source}", path.display())
            }
            Self::Sqlite { path, source } => write!(f, "cannot open {}: {source}", path.display()),
            Self::WriterIsReadOnly { path } => {
                write!(f, "{} can only be opened read-only", path.display())
            }
            Self::PragmaNotApplied {
                pragma,
                expected,
                found,
            } => write!(f, "PRAGMA {pragma} is {found}, expected {expected}"),
            Self::UnsupportedSchema(inner) => inner.fmt(f),
        }
    }
}

/// The message already includes the underlying error, so `source` is left empty
/// and a printed error chain says it once.
impl std::error::Error for OpenError {}

/// `<data dir>/callsheet.db`, in a form SQLite takes literally (see
/// [`sqlite_literal_path`]).
pub fn database_path(data_dir: &Path) -> PathBuf {
    sqlite_literal_path(&data_dir.join(DATABASE_FILE_NAME))
}

/// Opens the writer connection, creating an empty owner-only database file first
/// if there is none.
///
/// Refuses a database whose schema is newer than this build knows before any
/// pragma runs, reading its version through a read-only connection, which never
/// writes to the file (not even a checkpoint on close). The schema itself is
/// brought up to date by `crate::migrate::migrate`.
pub fn open_writer(data_dir: &Path) -> Result<Connection, OpenError> {
    let path = database_path(data_dir);
    create_owner_only_if_missing(&path)?;
    refuse_unsupported_schema(&path)?;

    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | common_flags();
    let conn = open(&path, flags)?;
    if conn.is_readonly(MAIN_DB).map_err(sqlite_error(&path))? {
        return Err(OpenError::WriterIsReadOnly { path });
    }
    apply_pragmas(&conn, &path)?;
    Ok(conn)
}

/// Opens a read-only connection. Open it after [`open_writer`], once the database
/// is in WAL mode.
pub fn open_reader(data_dir: &Path) -> Result<Connection, OpenError> {
    let path = database_path(data_dir);
    let conn = open_read_only(&path)?;
    apply_pragmas(&conn, &path)?;
    Ok(conn)
}

/// The rusqlite bundled build defines `SQLITE_USE_URI`, so SQLite reads a file
/// name starting with `file:` as a URI, even in `ATTACH` and `VACUUM INTO`. A
/// relative path gets a leading `./` so it is always taken as a plain path.
pub(crate) fn sqlite_literal_path(path: &Path) -> PathBuf {
    match path.components().next() {
        Some(Component::Normal(_)) => Path::new(".").join(path),
        _ => path.to_path_buf(),
    }
}

/// Flags for both connections: one connection per thread, and never follow a
/// symbolic link planted where the database should be.
fn common_flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW
}

fn open(path: &Path, flags: OpenFlags) -> Result<Connection, OpenError> {
    Connection::open_with_flags(path, flags).map_err(sqlite_error(path))
}

fn open_read_only(path: &Path) -> Result<Connection, OpenError> {
    open(path, OpenFlags::SQLITE_OPEN_READ_ONLY | common_flags())
}

fn create_owner_only_if_missing(path: &Path) -> Result<(), OpenError> {
    match fsperm::create_new_owner_only(path) {
        Ok(_file) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(OpenError::CreateFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn refuse_unsupported_schema(path: &Path) -> Result<(), OpenError> {
    let probe = open_read_only(path)?;
    let found = migrate::schema_version(&probe).map_err(sqlite_error(path))?;
    migrate::ensure_supported(found).map_err(OpenError::UnsupportedSchema)?;
    Ok(())
}

/// Sets the four pragmas and reads each one back.
fn apply_pragmas(conn: &Connection, path: &Path) -> Result<(), OpenError> {
    // Returns the journal mode now in effect.
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(sqlite_error(path))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(OpenError::PragmaNotApplied {
            pragma: "journal_mode",
            expected: "wal",
            found: mode,
        });
    }
    // FULL reads back as 2 (`getSafetyLevel` in `sqlite3.c`).
    set_and_check(conn, path, "synchronous", "FULL", 2)?;
    set_and_check(conn, path, "foreign_keys", "ON", 1)?;
    set_and_check(conn, path, "secure_delete", "ON", 1)
}

fn set_and_check(
    conn: &Connection,
    path: &Path,
    pragma: &'static str,
    value: &'static str,
    expected: i64,
) -> Result<(), OpenError> {
    conn.pragma_update(None, pragma, value)
        .map_err(sqlite_error(path))?;
    let found: i64 = conn
        .pragma_query_value(None, pragma, |row| row.get(0))
        .map_err(sqlite_error(path))?;
    if found == expected {
        Ok(())
    } else {
        Err(OpenError::PragmaNotApplied {
            pragma,
            expected: value,
            found: found.to_string(),
        })
    }
}

fn sqlite_error(path: &Path) -> impl FnOnce(rusqlite::Error) -> OpenError + '_ {
    move |source| OpenError::Sqlite {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pragma_i64(conn: &Connection, name: &str) -> i64 {
        conn.pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    fn assert_pragmas(conn: &Connection) {
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(pragma_i64(conn, "synchronous"), 2);
        assert_eq!(pragma_i64(conn, "foreign_keys"), 1);
        assert_eq!(pragma_i64(conn, "secure_delete"), 1);
    }

    fn write_a_setting(conn: &Connection) {
        conn.execute_batch("CREATE TABLE t (k TEXT); INSERT INTO t VALUES ('v');")
            .unwrap();
    }

    #[test]
    fn writer_and_reader_read_back_every_pragma() {
        let dir = tempfile::tempdir().unwrap();

        let writer = open_writer(dir.path()).unwrap();
        let reader = open_reader(dir.path()).unwrap();

        assert_pragmas(&writer);
        assert_pragmas(&reader);
    }

    #[test]
    fn wal_file_appears_after_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let writer = open_writer(dir.path()).unwrap();

        write_a_setting(&writer);

        let wal = dir.path().join("callsheet.db-wal");
        assert!(std::fs::metadata(&wal).unwrap().len() > 0);
        assert!(dir.path().join("callsheet.db-shm").exists());
    }

    #[test]
    fn reader_sees_committed_writes_and_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let writer = open_writer(dir.path()).unwrap();
        write_a_setting(&writer);
        let reader = open_reader(dir.path()).unwrap();

        let value: String = reader
            .query_row("SELECT k FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "v");
        assert!(reader.is_readonly(MAIN_DB).unwrap());
        let error = reader
            .execute("INSERT INTO t VALUES ('w')", [])
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
    }

    #[test]
    fn reopening_keeps_the_existing_database() {
        let dir = tempfile::tempdir().unwrap();
        write_a_setting(&open_writer(dir.path()).unwrap());

        let writer = open_writer(dir.path()).unwrap();

        let count: i64 = writer
            .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn a_newer_schema_is_refused_before_any_pragma() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DATABASE_FILE_NAME);
        {
            let conn = open_writer(dir.path()).unwrap();
            conn.pragma_update(None, "user_version", migrate::CURRENT_SCHEMA_VERSION + 1)
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();

        let error = open_writer(dir.path()).unwrap_err();

        assert!(matches!(
            error,
            OpenError::UnsupportedSchema(UnsupportedSchema { found, .. })
                if found == i64::from(migrate::CURRENT_SCHEMA_VERSION) + 1
        ));
        assert!(
            std::fs::read(&path).unwrap() == before,
            "database file changed"
        );
    }

    /// A newer daemon that crashed leaves committed frames in `-wal`. A read-write
    /// connection would checkpoint them into the main file when it closes; the
    /// read-only probe must not.
    #[test]
    fn a_newer_schema_with_a_hot_wal_leaves_both_files_unchanged() {
        let crashed = tempfile::tempdir().unwrap();
        let conn = open_writer(crashed.path()).unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        write_a_setting(&conn);
        conn.pragma_update(None, "user_version", migrate::CURRENT_SCHEMA_VERSION + 1)
            .unwrap();
        let copy = tempfile::tempdir().unwrap();
        for name in ["callsheet.db", "callsheet.db-wal"] {
            std::fs::copy(crashed.path().join(name), copy.path().join(name)).unwrap();
        }
        drop(conn);
        let read = |name: &str| std::fs::read(copy.path().join(name)).unwrap();
        let (db_before, wal_before) = (read("callsheet.db"), read("callsheet.db-wal"));
        assert!(!wal_before.is_empty());

        let error = open_writer(copy.path()).unwrap_err();

        assert!(matches!(error, OpenError::UnsupportedSchema(_)));
        assert!(read("callsheet.db") == db_before, "database file changed");
        assert!(read("callsheet.db-wal") == wal_before, "WAL file changed");
    }

    #[test]
    fn relative_paths_are_never_read_as_uris() {
        assert_eq!(
            sqlite_literal_path(Path::new("file:data/callsheet.db")),
            Path::new("./file:data/callsheet.db")
        );
        assert_eq!(
            sqlite_literal_path(Path::new("/tmp/callsheet.db")),
            Path::new("/tmp/callsheet.db")
        );
        assert_eq!(
            sqlite_literal_path(Path::new("./callsheet.db")),
            Path::new("./callsheet.db")
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_and_side_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let writer = open_writer(dir.path()).unwrap();
        write_a_setting(&writer);

        for name in ["callsheet.db", "callsheet.db-wal", "callsheet.db-shm"] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_in_place_of_the_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere.db");
        std::fs::write(&elsewhere, b"").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.path().join(DATABASE_FILE_NAME)).unwrap();

        assert!(matches!(
            open_writer(dir.path()),
            Err(OpenError::Sqlite { .. })
        ));
    }
}
