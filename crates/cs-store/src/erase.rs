//! Erasing content while keeping every event and hash (R6, ADR 0011).
//!
//! Owned by unit `erase`. The store facade ([`crate::Store::erase_plan`],
//! [`crate::Store::erase`], [`crate::Store::erasure_pending`]) and the writer
//! command live in `crate::writer`; the plan, the SQL, the checkpoint and the
//! pending-erasure bookkeeping live here.
//!
//! # Plan (dry run, R6.1)
//! [`plan_in`] reads, for one run: the distinct content addresses its events
//! refer to (`event_content`), the other runs whose events refer to any of them,
//! and how many of those addresses still have a blob. The plan ID is
//! `hex(SHA-256(canonical_json({"addresses", "runId", "sharedWithRuns"})))` with
//! both lists sorted (byte order, the same as SQLite's `BINARY` collation), so the
//! same state always gives the same ID. A dry run changes nothing.
//!
//! # Erase (R6.2–R6.4)
//! One command on the writer thread, so no append can interleave:
//! 1. take the read gate exclusively (`blocking_write` on the writer thread),
//!    which waits for open reads and holds new ones back;
//! 2. recompute the plan on the writer connection and refuse with
//!    [`EraseError::PlanOutOfDate`] if its ID differs from the confirmed one (a
//!    new event may have made another run share the content since the plan);
//! 3. in one transaction: delete the blobs (`secure_delete=ON` overwrites them),
//!    append a [`CONTENT_ERASED_KIND`] event to the run whose body is exactly
//!    `{"addresses": [...], "affectedRuns": [...], "planId": "<hex>"}` (both lists
//!    sorted, `affectedRuns` being the other runs) and no content, and set the
//!    setting [`ERASURE_PENDING_SETTING`] to `"1"`;
//! 4. `PRAGMA wal_checkpoint(TRUNCATE)`, checking its result row (not busy, no
//!    frames left) and that the `-wal` file, if present, is 0 bytes;
//! 5. delete every `backup-v*.db` ([`crate::migrate::remove_migration_backups`]);
//! 6. delete the pending setting and release the gate.
//!
//! If step 4 or 5 fails, the content is already deleted from the database but a
//! copy may remain in the WAL or a backup: erase returns [`EraseError::Pending`]
//! (JSON-RPC 1004) and the setting stays. While it is set, the writer retries
//! steps 1, 4, 5 and 6 every [`ERASURE_RETRY_INTERVAL`] and once when the store
//! opens. A retry leaves the backup that the same open's migration made (if
//! any): it was made after the erasure committed, by `VACUUM INTO`, which copies
//! live rows only, so it holds none of the erased content, and startup still
//! needs it until the migrated database has verified. A new erasure removes it
//! too, since it may hold that erasure's content.
//!
//! **Plan IDs name addresses, not blobs.** Once erased, a run's addresses stay in
//! `event_content`, so its plan (and plan ID) stays the same. Confirming the
//! same plan ID again erases the same addresses again, including a blob that a
//! later event stored anew with the same content; it never erases content the
//! user wasn't shown, because an address identifies exactly one content.
//!
//! **Verification contract (shared with unit `verify`):** a missing blob counts
//! as erased if any later `content.erased` event, in any run, lists its address.
//! Events of the other runs are therefore covered by the erased run's event.
//!
//! **A run without content** is a valid erase: its plan lists no addresses, and
//! erasing deletes nothing and appends no event (there is nothing to record),
//! but still truncates the WAL and removes backups, as the plan's
//! `backupsToRemove` said. It reports `erasedItems: 0`.
//!
//! **Erasing again** is allowed: the plan lists the same addresses, with
//! `contentItems` counting only blobs that came back (new events with the same
//! content). It appends another `content.erased` event.
//!
//! Checkpoint semantics from the SQLite 3.53.2 source bundled by
//! `libsqlite3-sys` 0.38.2 (`sqlite3/sqlite3.c`): `walCheckpoint` truncates the
//! log with `sqlite3OsTruncate(pWal->pWalFd, 0)` only once every frame is
//! backfilled and no reader uses the WAL; otherwise `OP_Checkpoint` reports
//! `SQLITE_BUSY` as a result row `(1, log, checkpointed)` rather than an error.
//! After a successful TRUNCATE both frame counts are 0 (`sqlite3_wal_checkpoint_v2`
//! docs in the same file).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cs_core::event::{EventHashError, canonical_json};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::StoreError;
use crate::db::DATABASE_FILE_NAME;

/// The setting that is `"1"` while an erasure's WAL truncation or backup
/// removal hasn't succeeded yet; absent otherwise.
pub const ERASURE_PENDING_SETTING: &str = "erasure_pending";

/// The kind of the event that records an erasure.
pub const CONTENT_ERASED_KIND: &str = "content.erased";

/// How often the writer retries a pending erasure (PRIVACY.md: every 30 s).
pub const ERASURE_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// The shortest retry interval; a shorter one (including zero) is raised to
/// it, so failing retries never keep the writer from taking commands.
pub const MIN_ERASURE_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// What erasing a run's content would remove. Built by [`plan_in`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ErasePlan {
    pub(crate) run_id: String,
    /// Distinct content addresses the run's events refer to, sorted.
    pub(crate) addresses: Vec<String>,
    /// Other runs whose events refer to any of `addresses`, sorted.
    pub(crate) shared_with_runs: Vec<String>,
    /// How many of `addresses` still have a blob.
    pub(crate) content_items: u64,
}

impl ErasePlan {
    /// `hex(SHA-256(canonical_json({"addresses", "runId", "sharedWithRuns"})))`.
    pub(crate) fn plan_id(&self) -> Result<String, StoreError> {
        let identity = json!({
            "runId": self.run_id,
            "addresses": self.addresses,
            "sharedWithRuns": self.shared_with_runs,
        });
        let canonical = canonical_json(&identity)
            .map_err(|error| StoreError::Hash(EventHashError::Canonicalization(error)))?;
        Ok(hex::encode(Sha256::digest(&canonical)))
    }

    /// The body of the `content.erased` event, exactly
    /// `{"addresses", "affectedRuns", "planId"}`.
    pub(crate) fn erased_event_body(&self, plan_id: &str) -> Value {
        json!({
            "addresses": self.addresses,
            "affectedRuns": self.shared_with_runs,
            "planId": plan_id,
        })
    }
}

/// Why a plan or an erasure failed.
#[derive(Debug)]
pub enum EraseError {
    /// No run with that ID exists. JSON-RPC 1003.
    UnknownRun,
    /// The plan changed since it was shown; nothing was erased. JSON-RPC 1002.
    PlanOutOfDate,
    /// The content was deleted and the erasure recorded, but the WAL couldn't
    /// be truncated or a backup couldn't be removed yet; the writer retries.
    /// JSON-RPC 1004.
    Pending,
    Store(StoreError),
    /// Counting the backups for a plan failed.
    Io(io::Error),
}

impl fmt::Display for EraseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRun => f.write_str("no run with that ID exists"),
            Self::PlanOutOfDate => f.write_str(
                "the erase plan is out of date; nothing was erased. Ask for a new plan and \
                 confirm it",
            ),
            Self::Pending => f.write_str(
                "erasure pending: the content was deleted from the database, but a copy may \
                 remain until the write-ahead log is truncated and every migration backup is \
                 removed. Callsheet retries every 30 seconds and at the next start",
            ),
            Self::Store(error) => error.fmt(f),
            Self::Io(error) => write!(f, "cannot count the migration backups: {error}"),
        }
    }
}

/// The message already includes the underlying error.
impl std::error::Error for EraseError {}

impl From<StoreError> for EraseError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for EraseError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(StoreError::Sqlite(error))
    }
}

/// Builds the plan for `run_id` from `conn`; `None` if the run doesn't exist.
/// Reads only.
pub(crate) fn plan_in(conn: &Connection, run_id: &str) -> rusqlite::Result<Option<ErasePlan>> {
    let exists: bool = conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM runs WHERE run_id = ?1)")?
        .query_row([run_id], |row| row.get(0))?;
    if !exists {
        return Ok(None);
    }
    let mut addresses: Vec<String> = conn
        .prepare_cached(
            "SELECT DISTINCT ec.address FROM event_content ec
             JOIN events e ON e.global_pos = ec.global_pos
             WHERE e.run_id = ?1",
        )?
        .query_map([run_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    addresses.sort_unstable();

    let mut shared_with_runs: Vec<String> = conn
        .prepare_cached(
            "SELECT DISTINCT e.run_id FROM event_content ec
             JOIN events e ON e.global_pos = ec.global_pos
             WHERE e.run_id <> ?1 AND ec.address IN (
               SELECT mine.address FROM event_content mine
               JOIN events m ON m.global_pos = mine.global_pos
               WHERE m.run_id = ?1)",
        )?
        .query_map([run_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    shared_with_runs.sort_unstable();

    let content_items: i64 = conn
        .prepare_cached(
            "SELECT count(*) FROM blobs WHERE address IN (
               SELECT ec.address FROM event_content ec
               JOIN events e ON e.global_pos = ec.global_pos
               WHERE e.run_id = ?1)",
        )?
        .query_row([run_id], |row| row.get(0))?;
    let content_items = u64::try_from(content_items)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, content_items))?;

    Ok(Some(ErasePlan {
        run_id: run_id.to_owned(),
        addresses,
        shared_with_runs,
        content_items,
    }))
}

/// Deletes the blobs of `addresses` inside the caller's transaction and returns
/// how many existed.
pub(crate) fn delete_blobs(conn: &Connection, addresses: &[String]) -> rusqlite::Result<u64> {
    let mut delete = conn.prepare_cached("DELETE FROM blobs WHERE address = ?1")?;
    let mut deleted = 0;
    for address in addresses {
        deleted += u64::try_from(delete.execute([address])?).unwrap_or(0);
    }
    Ok(deleted)
}

/// Whether the pending setting is set.
pub(crate) fn read_pending(conn: &Connection) -> rusqlite::Result<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [ERASURE_PENDING_SETTING],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.as_deref() == Some("1"))
}

/// Sets the pending setting, inside the caller's transaction.
pub(crate) fn set_pending(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, '1')
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![ERASURE_PENDING_SETTING],
    )?;
    Ok(())
}

fn clear_pending(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        params![ERASURE_PENDING_SETTING],
    )?;
    Ok(())
}

/// Deletes the backups in a data directory, except the one with the given file
/// name, and returns how many it deleted. Replaced in tests to make removal
/// fail.
pub(crate) type BackupRemover = Arc<dyn Fn(&Path, Option<&OsStr>) -> io::Result<u64> + Send + Sync>;

/// The production [`BackupRemover`].
pub(crate) fn default_backup_remover() -> BackupRemover {
    Arc::new(crate::migrate::remove_migration_backups_except)
}

/// Why the steps after the erase transaction failed. Never holds content.
#[derive(Debug)]
pub(crate) enum FinishError {
    Checkpoint(rusqlite::Error),
    /// The checkpoint ran but was busy or left frames: `(busy, log frames)`.
    CheckpointIncomplete(i64, i64),
    /// The checkpoint reported success but the `-wal` file isn't empty.
    WalNotEmpty(u64),
    WalSize(io::Error),
    RemoveBackups(io::Error),
    ClearPending(rusqlite::Error),
}

impl fmt::Display for FinishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Checkpoint(error) => write!(f, "WAL checkpoint failed: {error}"),
            Self::CheckpointIncomplete(busy, log) => write!(
                f,
                "WAL checkpoint did not truncate the log (busy {busy}, {log} frames left)"
            ),
            Self::WalNotEmpty(len) => write!(f, "the WAL file is still {len} bytes"),
            Self::WalSize(error) => write!(f, "cannot read the WAL file size: {error}"),
            Self::RemoveBackups(error) => write!(f, "cannot remove a migration backup: {error}"),
            Self::ClearPending(error) => {
                write!(f, "cannot clear the erasure-pending setting: {error}")
            }
        }
    }
}

/// Finishes erasures on the writer thread and tracks whether one is pending.
pub(crate) struct Finisher {
    data_dir: PathBuf,
    remove_backups: BackupRemover,
    /// Shared with the store, for `health`.
    pending: Arc<AtomicBool>,
    retry_interval: Duration,
    next_retry: Instant,
    /// File name of the backup this open's migration made, left by retries of
    /// an erasure committed before it (see the module docs).
    fresh_backup: Option<OsString>,
}

impl Finisher {
    /// `fresh_backup` is the backup the migration at this open made, if any.
    /// `retry_interval` is raised to at least [`MIN_ERASURE_RETRY_INTERVAL`].
    pub(crate) fn new(
        data_dir: PathBuf,
        remove_backups: BackupRemover,
        pending: Arc<AtomicBool>,
        retry_interval: Duration,
        fresh_backup: Option<&Path>,
    ) -> Self {
        Self {
            data_dir,
            remove_backups,
            pending,
            retry_interval: retry_interval.max(MIN_ERASURE_RETRY_INTERVAL),
            // Due now: a pending erasure found at open is retried at once.
            next_retry: Instant::now(),
            fresh_backup: fresh_backup.and_then(Path::file_name).map(OsStr::to_owned),
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }

    /// Records that a new erase transaction committed with the setting set.
    /// From now on every backup goes, since any may hold this erasure's
    /// content.
    pub(crate) fn mark_pending(&mut self) {
        self.fresh_backup = None;
        self.pending.store(true, Ordering::SeqCst);
    }

    /// How long until the next retry is due; zero if it is due now.
    pub(crate) fn until_retry(&self) -> Duration {
        self.next_retry.saturating_duration_since(Instant::now())
    }

    /// Steps 4 to 6: truncate the WAL, remove the backups, clear the pending
    /// setting. Returns how many backups were removed. Call with the read gate
    /// held exclusively and no transaction open on `conn`. On failure the
    /// setting stays and the next retry is scheduled.
    pub(crate) fn finish(&mut self, conn: &Connection) -> Result<u64, FinishError> {
        let outcome = self.try_finish(conn);
        match &outcome {
            Ok(_) => self.pending.store(false, Ordering::SeqCst),
            Err(error) => {
                tracing::warn!(%error, "erasure pending; retrying");
                self.next_retry = Instant::now() + self.retry_interval;
            }
        }
        outcome
    }

    fn try_finish(&self, conn: &Connection) -> Result<u64, FinishError> {
        checkpoint_truncate(conn, &self.data_dir)?;
        let removed = (self.remove_backups)(&self.data_dir, self.fresh_backup.as_deref())
            .map_err(FinishError::RemoveBackups)?;
        clear_pending(conn).map_err(FinishError::ClearPending)?;
        Ok(removed)
    }
}

/// `PRAGMA wal_checkpoint(TRUNCATE)`, then checks the WAL is really empty.
fn checkpoint_truncate(conn: &Connection, data_dir: &Path) -> Result<(), FinishError> {
    let (busy, log) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(FinishError::Checkpoint)?;
    if busy != 0 || log != 0 {
        return Err(FinishError::CheckpointIncomplete(busy, log));
    }
    let wal = data_dir.join(format!("{DATABASE_FILE_NAME}-wal"));
    match fs::metadata(&wal) {
        Ok(metadata) if metadata.len() == 0 => Ok(()),
        Ok(metadata) => Err(FinishError::WalNotEmpty(metadata.len())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(FinishError::WalSize(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc as std_mpsc;

    use cs_core::event::{HashedEvent, event_hash};
    use rusqlite::OpenFlags;

    use super::*;
    use crate::content::{self, ContentKey};
    use crate::migrate;
    use crate::secrets::{InMemorySecretStore, KEY_LEN};
    use crate::writer::{AppendEvent, Store, StoreOptions};

    const KEY: [u8; KEY_LEN] = [7; KEY_LEN];
    const TS: u64 = 1_790_000_000_000;
    const WAIT: Duration = Duration::from_secs(10);

    type EventRow = (i64, String, i64, String, i64, String, String, String);

    fn options(retry: Duration) -> StoreOptions {
        StoreOptions {
            erasure_retry_interval: retry,
        }
    }

    fn secrets() -> Arc<InMemorySecretStore> {
        Arc::new(InMemorySecretStore::new(KEY))
    }

    async fn open(dir: &Path) -> Store {
        open_removing(dir, default_backup_remover(), ERASURE_RETRY_INTERVAL).await
    }

    async fn open_removing(dir: &Path, remover: BackupRemover, retry: Duration) -> Store {
        let store = Store::open_inner(dir, secrets(), options(retry), remover).unwrap();
        assert!(store.set_capture_content(true).await.unwrap());
        store
    }

    fn address(bytes: &[u8]) -> String {
        content::content_address(&ContentKey::take(&mut { KEY }), bytes)
    }

    async fn append(store: &Store, run: &str, n: u64, items: &[&[u8]]) {
        store
            .append(AppendEvent {
                run_id: run.to_owned(),
                kind: "test.event".to_owned(),
                ts_ms: TS + n,
                body: json!({ "n": n }),
                content: items.iter().map(|item| item.to_vec()).collect(),
            })
            .await
            .unwrap();
    }

    /// Runs a: alpha, beta; b: beta; c: gamma; d: no content.
    async fn seed(store: &Store) {
        append(store, "a", 1, &[b"alpha", b"beta"]).await;
        append(store, "b", 2, &[b"beta"]).await;
        append(store, "c", 3, &[b"gamma"]).await;
        append(store, "d", 4, &[]).await;
        append(store, "a", 5, &[b"alpha"]).await;
    }

    async fn events(store: &Store) -> Vec<EventRow> {
        store
            .read(|conn| {
                conn.prepare(
                    "SELECT global_pos, run_id, seq, kind, ts_ms, body, prev_hash, event_hash
                     FROM events ORDER BY global_pos",
                )?
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                })?
                .collect()
            })
            .await
            .unwrap()
    }

    async fn blob_addresses(store: &Store) -> Vec<String> {
        store
            .read(|conn| {
                conn.prepare("SELECT address FROM blobs ORDER BY address")?
                    .query_map([], |row| row.get(0))?
                    .collect()
            })
            .await
            .unwrap()
    }

    async fn settings(store: &Store) -> Vec<(String, String)> {
        store
            .read(|conn| {
                conn.prepare("SELECT key, value FROM settings ORDER BY key")?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .await
            .unwrap()
    }

    async fn pending_setting(store: &Store) -> bool {
        store.read(read_pending).await.unwrap()
    }

    fn sorted(mut items: Vec<String>) -> Vec<String> {
        items.sort();
        items
    }

    fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    /// A `VACUUM INTO` copy of the database, as a migration backup would be.
    fn backup_copy(dir: &Path, name: &str) {
        let conn = Connection::open_with_flags(
            dir.join(DATABASE_FILE_NAME),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let target = dir.join(name);
        conn.execute("VACUUM INTO ?1", [target.to_str().unwrap()])
            .unwrap();
        conn.close().unwrap();
    }

    async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn plan_lists_shared_runs_and_counts_what_would_go() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        seed(&store).await;
        fs::write(dir.path().join("backup-v0.db"), b"old").unwrap();
        fs::create_dir(dir.path().join("backup-v9.db")).unwrap();

        let a = store.erase_plan("a").await.unwrap();
        assert_eq!(a.run_id, "a");
        assert_eq!(a.shared_with_runs, ["b"]);
        assert_eq!(a.content_items, 2);
        assert_eq!(a.backups_to_remove, 1, "only regular backup-v*.db files");
        // Pinned: sorted lists, canonical JSON (keys sorted), SHA-256, hex.
        let addresses = sorted(vec![address(b"alpha"), address(b"beta")]);
        let expected = format!(
            r#"{{"addresses":["{}","{}"],"runId":"a","sharedWithRuns":["b"]}}"#,
            addresses[0], addresses[1]
        );
        assert_eq!(a.plan_id, hex::encode(Sha256::digest(expected.as_bytes())));
        assert_eq!(store.erase_plan("a").await.unwrap(), a, "plans are stable");

        let b = store.erase_plan("b").await.unwrap();
        assert_eq!(
            (b.shared_with_runs, b.content_items),
            (vec!["a".to_owned()], 1)
        );
        let c = store.erase_plan("c").await.unwrap();
        assert_eq!((c.shared_with_runs, c.content_items), (Vec::new(), 1));
        let d = store.erase_plan("d").await.unwrap();
        assert_eq!((d.shared_with_runs, d.content_items), (Vec::new(), 0));
    }

    /// Verification trusts `content.erased` to explain missing blobs, so no
    /// caller of `append` may forge one.
    #[tokio::test]
    async fn append_refuses_the_content_erased_kind() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        seed(&store).await;
        let before = events(&store).await;

        let forged = store
            .append(AppendEvent {
                run_id: "b".to_owned(),
                kind: CONTENT_ERASED_KIND.to_owned(),
                ts_ms: TS,
                body: json!({ "addresses": [address(b"beta")], "affectedRuns": [], "planId": "x" }),
                content: Vec::new(),
            })
            .await;

        assert!(
            matches!(
                forged,
                Err(StoreError::InvalidEvent(crate::InvalidEvent::ReservedKind))
            ),
            "{forged:?}"
        );
        assert_eq!(events(&store).await, before, "nothing appended");
    }

    #[tokio::test]
    async fn a_dry_run_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        seed(&store).await;
        let before = (
            events(&store).await,
            blob_addresses(&store).await,
            settings(&store).await,
        );

        for run in ["a", "b", "c", "d"] {
            store.erase_plan(run).await.unwrap();
        }

        let after = (
            events(&store).await,
            blob_addresses(&store).await,
            settings(&store).await,
        );
        assert_eq!(after, before);
        assert!(!store.erasure_pending());
    }

    #[tokio::test]
    async fn an_unknown_run_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        seed(&store).await;
        let before = events(&store).await;

        assert!(matches!(
            store.erase_plan("nope").await,
            Err(EraseError::UnknownRun)
        ));
        let plan_id = store.erase_plan("a").await.unwrap().plan_id;
        assert!(matches!(
            store.erase("nope", &plan_id).await,
            Err(EraseError::UnknownRun)
        ));
        assert_eq!(events(&store).await, before);
        assert!(!pending_setting(&store).await);
    }

    #[tokio::test]
    async fn a_stale_plan_is_refused_and_nothing_is_erased() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        seed(&store).await;
        let plan = store.erase_plan("a").await.unwrap();
        assert_eq!(plan.shared_with_runs, ["b"]);

        // A third run starts sharing the content after the user saw the plan.
        append(&store, "e", 6, &[b"alpha"]).await;
        let before = (events(&store).await, blob_addresses(&store).await);

        let refused = store.erase("a", &plan.plan_id).await;
        assert!(
            matches!(refused, Err(EraseError::PlanOutOfDate)),
            "{refused:?}"
        );
        if let Err(error) = refused {
            println!("stale plan: {error}");
        }
        assert_eq!(
            (events(&store).await, blob_addresses(&store).await),
            before,
            "nothing erased, no event appended"
        );
        assert!(!store.erasure_pending());
        assert!(!pending_setting(&store).await);

        let fresh = store.erase_plan("a").await.unwrap();
        assert_eq!(fresh.shared_with_runs, ["b", "e"]);
        assert_ne!(fresh.plan_id, plan.plan_id);
        let erased = store.erase("a", &fresh.plan_id).await.unwrap();
        assert_eq!(erased.affected_runs, ["b", "e"]);
    }

    #[tokio::test]
    async fn erasing_removes_every_copy_and_keeps_every_event() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        seed(&store).await;
        let before = events(&store).await;
        let plan = store.erase_plan("a").await.unwrap();

        let erased = store.erase("a", &plan.plan_id).await.unwrap();

        assert_eq!(erased.erased_items, 2);
        assert_eq!(erased.affected_runs, ["b"]);
        assert_eq!(erased.backups_removed, 0);
        // Run b's copy of "beta" went too; run c's content stays.
        assert_eq!(blob_addresses(&store).await, [address(b"gamma")]);

        let after = events(&store).await;
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(after[..before.len()], before[..], "every event unchanged");
        let (pos, run, seq, kind, ts_ms, body, prev_hash, hash) = after[before.len()].clone();
        assert_eq!((run.as_str(), kind.as_str()), ("a", CONTENT_ERASED_KIND));
        let addresses = sorted(vec![address(b"alpha"), address(b"beta")]);
        let expected_body = json!({
            "addresses": addresses,
            "affectedRuns": ["b"],
            "planId": plan.plan_id,
        });
        assert_eq!(
            body,
            String::from_utf8(canonical_json(&expected_body).unwrap()).unwrap(),
            "exact body, no content member"
        );
        let a_last = before.iter().rfind(|row| row.1 == "a").unwrap();
        assert_eq!((seq, &prev_hash), (a_last.2 + 1, &a_last.7));
        let recomputed = event_hash(&HashedEvent {
            run_id: &run,
            seq: u64::try_from(seq).unwrap(),
            global_pos: u64::try_from(pos).unwrap(),
            kind: &kind,
            ts_ms: u64::try_from(ts_ms).unwrap(),
            body: &expected_body,
            prev_hash: &prev_hash,
        })
        .unwrap();
        assert_eq!(hash, recomputed);

        assert!(!store.erasure_pending());
        assert!(!pending_setting(&store).await);
        let again = store.erase_plan("a").await.unwrap();
        assert_eq!((again.content_items, again.plan_id), (0, plan.plan_id));
    }

    #[tokio::test]
    async fn erasing_a_run_without_content_appends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        seed(&store).await;
        let before = (events(&store).await, blob_addresses(&store).await);
        let plan = store.erase_plan("d").await.unwrap();

        let erased = store.erase("d", &plan.plan_id).await.unwrap();

        assert_eq!(erased.erased_items, 0);
        assert!(erased.affected_runs.is_empty());
        assert_eq!((events(&store).await, blob_addresses(&store).await), before);
        assert!(!pending_setting(&store).await);
    }

    /// R6.3: after erasing, the content's bytes are in no file of the data
    /// directory: not the database, not the WAL, not a migration backup.
    #[tokio::test]
    async fn erased_bytes_are_in_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path()).await;
        let mut random = [0u8; 64];
        getrandom::fill(&mut random).unwrap();
        let marker = format!("callsheet-erase-marker-{}", hex::encode(random));
        // Large enough for overflow pages, plus a small one in a later event.
        let large = marker.repeat(200);
        let small = format!("second:{marker}");
        append(&store, "a", 1, &[large.as_bytes()]).await;
        append(&store, "other", 2, &[b"unrelated"]).await;
        // Move the first copy into the database file itself; the second then
        // lives only in the WAL.
        {
            let conn = Connection::open(dir.path().join(DATABASE_FILE_NAME)).unwrap();
            let busy: i64 = conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
                .unwrap();
            assert_eq!(busy, 0);
            conn.close().unwrap();
        }
        append(&store, "a", 3, &[small.as_bytes()]).await;
        append(&store, "b", 4, &[small.as_bytes()]).await;
        backup_copy(dir.path(), "backup-v0.db");

        let needle = marker.as_bytes();
        let count_in = |name: &str| {
            fs::read(dir.path().join(name))
                .map(|bytes| occurrences(&bytes, needle))
                .unwrap_or(0)
        };
        for name in [DATABASE_FILE_NAME, "callsheet.db-wal", "backup-v0.db"] {
            let found = count_in(name);
            println!("byte search before erasing: {name}: marker found {found} times");
            assert!(found > 0, "the search finds the copy in {name}");
        }

        let plan = store.erase_plan("a").await.unwrap();
        assert_eq!(plan.backups_to_remove, 1);
        let erased = store.erase("a", &plan.plan_id).await.unwrap();
        assert_eq!(erased.erased_items, 2);
        assert_eq!(erased.affected_runs, ["b"]);
        assert_eq!(erased.backups_removed, 1);
        assert_eq!(blob_addresses(&store).await, [address(b"unrelated")]);

        let mut searched = Vec::new();
        for entry in fs::read_dir(dir.path()).unwrap() {
            let entry = entry.unwrap();
            assert!(entry.file_type().unwrap().is_file(), "only files here");
            let bytes = fs::read(entry.path()).unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let found = occurrences(&bytes, needle);
            println!(
                "byte search: {name} ({} bytes): marker found {found} times",
                bytes.len()
            );
            assert_eq!(found, 0, "erased content found in {name}");
            searched.push(name);
        }
        assert!(searched.iter().any(|name| name == DATABASE_FILE_NAME));
        assert!(!searched.iter().any(|name| name.starts_with("backup-v")));
    }

    /// Erase waits for an open read, and reads asked for meanwhile wait for
    /// the erase.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn erase_waits_for_open_readers_and_holds_new_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(open(dir.path()).await);
        seed(&store).await;
        let plan = store.erase_plan("a").await.unwrap();

        let (started, reading) = tokio::sync::oneshot::channel();
        let (release, released) = std_mpsc::channel::<()>();
        let held = tokio::spawn({
            let store = Arc::clone(&store);
            async move {
                store
                    .read(move |conn| {
                        let _ = started.send(());
                        let _ = released.recv();
                        content::blob_count(conn)
                    })
                    .await
            }
        });
        reading.await.unwrap();

        let erase = tokio::spawn({
            let store = Arc::clone(&store);
            let plan_id = plan.plan_id.clone();
            async move { store.erase("a", &plan_id).await }
        });
        let queued = Arc::clone(&store);
        wait_until("the erase waits for the gate", || {
            queued.readers_held_back()
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!erase.is_finished(), "erase must wait for the open read");
        println!("reader-wait: erase is waiting while a read is open");

        let late = tokio::spawn({
            let store = Arc::clone(&store);
            async move { store.blob_count().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!late.is_finished(), "a new read waits behind the erase");

        release.send(()).unwrap();
        let seen_by_held = held.await.unwrap().unwrap();
        let erased = erase.await.unwrap().unwrap();
        let seen_by_late = late.await.unwrap().unwrap();
        println!(
            "reader-wait: the open read saw {seen_by_held} blobs, the erase removed {}, \
             the read queued behind it saw {seen_by_late}",
            erased.erased_items
        );
        assert_eq!(seen_by_held, 3, "the open read ran before the erase");
        assert_eq!(erased.erased_items, 2);
        assert_eq!(seen_by_late, 1, "the queued read ran after the erase");
    }

    #[tokio::test]
    async fn a_failed_backup_removal_is_pending_until_a_retry_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let obstacle = Arc::new(AtomicBool::new(true));
        let remover: BackupRemover = {
            let obstacle = Arc::clone(&obstacle);
            Arc::new(move |dir: &Path, keep: Option<&OsStr>| {
                if obstacle.load(Ordering::SeqCst) {
                    Err(io::Error::other("backup is in use"))
                } else {
                    migrate::remove_migration_backups_except(dir, keep)
                }
            })
        };
        let store = open_removing(dir.path(), remover, Duration::from_millis(50)).await;
        seed(&store).await;
        backup_copy(dir.path(), "backup-v0.db");
        let plan = store.erase_plan("a").await.unwrap();

        let outcome = store.erase("a", &plan.plan_id).await;

        assert!(matches!(outcome, Err(EraseError::Pending)), "{outcome:?}");
        assert!(store.erasure_pending());
        assert!(pending_setting(&store).await);
        // The content itself is already deleted and the erasure recorded.
        assert_eq!(blob_addresses(&store).await, [address(b"gamma")]);
        let last = events(&store).await.pop().unwrap();
        assert_eq!(last.3, CONTENT_ERASED_KIND);
        assert!(dir.path().join("backup-v0.db").exists());
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(store.erasure_pending(), "still pending while removal fails");

        obstacle.store(false, Ordering::SeqCst);
        let retried = &store;
        wait_until("the retry clears the pending erasure", || {
            !retried.erasure_pending()
        })
        .await;
        assert!(!pending_setting(&store).await);
        assert!(!dir.path().join("backup-v0.db").exists());
    }

    /// A retry leaves the backup this open's migration made (it holds no
    /// erased content and startup still needs it); a new erasure removes it.
    #[test]
    fn retries_keep_the_fresh_migration_backup_until_a_new_erasure() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = crate::db::open_writer(dir.path()).unwrap();
        migrate::migrate(&mut conn, dir.path()).unwrap();
        let fresh = migrate::backup_path(dir.path(), 1);
        let old = migrate::backup_path(dir.path(), 0);
        fs::write(&fresh, b"fresh").unwrap();
        fs::write(&old, b"old").unwrap();
        set_pending(&conn).unwrap();
        let pending = Arc::new(AtomicBool::new(true));
        let mut finisher = Finisher::new(
            dir.path().to_path_buf(),
            default_backup_remover(),
            Arc::clone(&pending),
            Duration::ZERO,
            Some(&fresh),
        );
        assert_eq!(finisher.retry_interval, MIN_ERASURE_RETRY_INTERVAL);

        assert_eq!(finisher.finish(&conn).unwrap(), 1, "the retry");
        assert_eq!((fresh.exists(), old.exists()), (true, false));
        assert!(!pending.load(Ordering::SeqCst));
        assert!(!read_pending(&conn).unwrap());

        finisher.mark_pending();
        assert_eq!(finisher.finish(&conn).unwrap(), 1, "a new erasure");
        assert!(!fresh.exists());
    }

    #[tokio::test]
    async fn a_pending_erasure_is_retried_when_the_store_opens() {
        let dir = tempfile::tempdir().unwrap();
        let failing: BackupRemover =
            Arc::new(|_: &Path, _: Option<&OsStr>| Err(io::Error::other("stuck")));
        let hour = Duration::from_secs(3600);
        {
            let store = open_removing(dir.path(), failing, hour).await;
            seed(&store).await;
            backup_copy(dir.path(), "backup-v0.db");
            let plan = store.erase_plan("a").await.unwrap();
            assert!(matches!(
                store.erase("a", &plan.plan_id).await,
                Err(EraseError::Pending)
            ));
            store.close().await.unwrap();
        }

        let store = Store::open_with(dir.path(), secrets(), options(hour)).unwrap();
        let reopened = &store;
        wait_until("the retry at open clears the pending erasure", || {
            !reopened.erasure_pending()
        })
        .await;
        assert!(!pending_setting(&store).await);
        assert!(!dir.path().join("backup-v0.db").exists());
    }
}
