//! The store facade: the single writer thread, event append, the read gate and
//! the `capture_content` setting (R3.4, R4.1–R4.4, R5.1, R5.2).
//!
//! Owned by unit `writer`.
//!
//! # Opening
//! [`Store::open`] is synchronous and blocks on file I/O: it runs
//! [`db::open_writer`], [`migrate::migrate`] and [`db::open_reader`], in that
//! order, then starts the writer thread. Call it at startup or from
//! `tokio::task::spawn_blocking`. What migrating did stays available from
//! [`Store::migrated`], so startup can delete the backup once it has verified.
//! Every other method is `async` and must run inside a Tokio runtime.
//!
//! # Writes
//! One writer connection lives on a dedicated `std::thread`. It takes commands
//! from a bounded `tokio::sync::mpsc` queue with `blocking_recv` and answers each
//! on its own `oneshot`. Every write goes through it, which is what keeps `seq`
//! and `global_pos` gap-free under concurrent callers (R4.2, R4.3). Dropping the
//! [`Store`] or calling [`Store::close`] closes the queue: commands already queued
//! are still carried out, then the thread closes its connection and ends.
//! `close` also waits for that.
//! A caller that stops waiting for its reply (a dropped future) doesn't undo its
//! command: once queued, an append commits.
//!
//! # Append
//! [`Store::append`] checks the event before it is queued ([`InvalidEvent`]) and
//! then, in one `BEGIN IMMEDIATE` transaction on the writer thread: creates the
//! `runs` row on a run's first event (`created_ms` is that event's `ts_ms`),
//! takes `seq = last_seq + 1` and `prev_hash = last_hash` (1 and `ZERO_HASH` for
//! a new run) and `global_pos = max + 1`, checks that link, stores content (see
//! below), hashes with [`cs_core::event::event_hash`], inserts the event with its
//! canonical JSON body and moves `runs.last_seq`/`last_hash` on. Any failure
//! rolls the whole transaction back. The database triggers refuse every UPDATE
//! and DELETE of an event (R4.1); nothing here tries one.
//!
//! # Content (R4.4, R5.1, R5.2)
//! An event's `content` items are message bytes, never part of the body; a body
//! may therefore not have a top-level `content` member of its own.
//! - **Capture off:** the bytes are ignored. No blob, no address, no
//!   `event_content` row; the body is stored exactly as given.
//! - **Capture on:** each item is stored with [`content::put_content`] and the
//!   body gets `"content": [address, …]`, one address per item in the order
//!   given (duplicates stay, so the list mirrors the items), before hashing.
//!   `event_content` gets one row per distinct address. Identical bytes, in any
//!   event of any run, are stored once. An event with no items gets no `content`
//!   member.
//!
//! # The capture setting and the content key (R3.4, R5.3, R5.4)
//! `settings.capture_content` is `"true"` or `"false"`; absent means off.
//! Enabling first gets the key from the [`SecretStore`] on a blocking thread
//! (`spawn_blocking`): never on the writer thread, which must not wait on an
//! unlock prompt, and never directly in an async task, where zbus panics. The
//! key may be generated ([`IfMissing::Generate`]) only while no blob is stored;
//! otherwise a missing key is an error ([`IfMissing::Fail`]), because a new key
//! would orphan every stored blob. If the key can't be had, capture stays off
//! (a setting left on by an earlier run is turned off) and
//! [`StoreError::Keychain`] (code 1001) is returned. On success the setting
//! is written and the writer thread keeps the key (a [`ContentKey`], zeroed on
//! drop); disabling drops it.
//!
//! **Opening with capture already on** loads the key lazily, on the first
//! append that carries content, not at open: startup must not wait on a
//! keychain unlock prompt, and appends without content never need the key. If
//! the key can't be had then, that append fails with [`StoreError::Keychain`]
//! and nothing is written. Storing the event without its content would be
//! silent data loss for a user who asked for capture, and a later retry (after
//! unlocking the keychain) can still record it. Appends without content go on
//! working, and the setting keeps saying what the user chose. After a failed
//! attempt, appends with content fail at once for [`KEY_RETRY_INTERVAL`], so a
//! burst of them asks the keychain once rather than prompting once per event.
//!
//! # Reads
//! One read-only connection behind a `std::sync::Mutex` serves every read, so
//! reads run one at a time on a blocking thread. [`Store::read`] holds the read
//! gate (`tokio::sync::RwLock`) shared for the whole SQLite read transaction;
//! [`Store::exclude_readers`] takes it exclusively, which waits for open reads to
//! finish and holds new ones back (erasure, task 10).

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use cs_core::event::{self, BodyError, EventHashError, HashedEvent, MAX_SAFE_INTEGER, ZERO_HASH};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use tokio::sync::{OwnedRwLockWriteGuard, RwLock, mpsc, oneshot, watch};

use crate::content::{self, ContentKey};
use crate::db::{self, OpenError};
use crate::migrate::{self, MigrateError, Migrated};
use crate::secrets::{IfMissing, KeychainUnavailable, SecretStore};

/// The settings key of the capture setting.
pub const CAPTURE_CONTENT_SETTING: &str = "capture_content";

/// The body member that lists an event's content addresses.
pub const CONTENT_FIELD: &str = "content";

/// Longest `run_id` or `kind`, in bytes.
pub const MAX_NAME_LEN: usize = 256;

/// How many commands may wait for the writer thread before senders wait.
pub const QUEUE_CAPACITY: usize = 256;

/// After the content key could not be loaded for an append, appends with
/// content fail at once for this long instead of asking the keychain again.
pub const KEY_RETRY_INTERVAL: Duration = Duration::from_secs(10);

const WRITER_THREAD_NAME: &str = "cs-store-writer";

/// An event to append.
#[derive(Clone, PartialEq)]
pub struct AppendEvent {
    /// The run's chain; created on its first event. 1 to [`MAX_NAME_LEN`] bytes.
    pub run_id: String,
    /// 1 to [`MAX_NAME_LEN`] bytes.
    pub kind: String,
    /// Unix milliseconds, at most 2^53 − 1.
    pub ts_ms: u64,
    /// A JSON object without a top-level `content` member, whose numbers are
    /// all integers within ±(2^53 − 1) (`cs_core::event::check_body_numbers`).
    pub body: Value,
    /// Message content. Ignored while capture is off; stored by address while
    /// it is on (see the module docs).
    pub content: Vec<Vec<u8>>,
}

/// Shows the size of each content item, never its bytes.
impl fmt::Debug for AppendEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let content_lens: Vec<usize> = self.content.iter().map(Vec::len).collect();
        f.debug_struct("AppendEvent")
            .field("run_id", &self.run_id)
            .field("kind", &self.kind)
            .field("ts_ms", &self.ts_ms)
            .field("body", &self.body)
            .field("content_lens", &content_lens)
            .finish()
    }
}

/// Where an appended event landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendedEvent {
    pub run_id: String,
    /// Position in the run's chain, from 1.
    pub seq: u64,
    /// Position in the global commit order, from 1.
    pub global_pos: u64,
    pub event_hash: String,
}

/// Why an event was refused before anything was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidEvent {
    EmptyRunId,
    RunIdTooLong,
    EmptyKind,
    KindTooLong,
    /// `ts_ms` is above 2^53 − 1.
    TimestampOutOfRange,
    BodyNotAnObject,
    /// The body has a top-level `content` member; that name is reserved for
    /// content addresses the store adds.
    BodyHasContentField,
    /// A number the hash path doesn't allow, or nesting too deep.
    Body(BodyError),
}

impl fmt::Display for InvalidEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRunId => f.write_str("event runId is empty"),
            Self::RunIdTooLong => write!(f, "event runId is longer than {MAX_NAME_LEN} bytes"),
            Self::EmptyKind => f.write_str("event kind is empty"),
            Self::KindTooLong => write!(f, "event kind is longer than {MAX_NAME_LEN} bytes"),
            Self::TimestampOutOfRange => f.write_str("event tsMs is above 2^53 − 1"),
            Self::BodyNotAnObject => f.write_str("event body is not a JSON object"),
            Self::BodyHasContentField => write!(
                f,
                "event body has a top-level \"{CONTENT_FIELD}\" member, which is reserved for \
                 content addresses"
            ),
            Self::Body(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for InvalidEvent {}

/// Why a store operation failed.
#[derive(Debug)]
pub enum StoreError {
    /// The event was refused; nothing was written.
    InvalidEvent(InvalidEvent),
    /// The content key isn't available, so capture stays off (enabling) or the
    /// event wasn't written (an append with content). JSON-RPC code 1001.
    Keychain(KeychainUnavailable),
    /// The stored chain can't be extended (a run's last hash is malformed, or a
    /// position would pass 2^53 − 1). Nothing was written.
    Chain(&'static str),
    /// Hashing refused the event; nothing was written.
    Hash(EventHashError),
    Sqlite(rusqlite::Error),
    /// The store was closed; nothing more can be written.
    Closed,
    /// The writer thread stopped without the store being closed (it
    /// panicked); nothing more can be written.
    WriterFailed,
    /// A blocking task (a read or a keychain call) panicked or was cancelled.
    TaskFailed,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEvent(error) => error.fmt(f),
            Self::Keychain(error) => error.fmt(f),
            Self::Chain(what) => write!(f, "cannot extend the event log: {what}"),
            Self::Hash(error) => write!(f, "cannot hash the event: {error}"),
            Self::Sqlite(error) => write!(f, "database error: {error}"),
            Self::Closed => f.write_str("the store is closed"),
            Self::WriterFailed => f.write_str("the store's writer thread has stopped"),
            Self::TaskFailed => f.write_str("a store task failed before finishing"),
        }
    }
}

/// The message already includes the underlying error, so `source` is left empty
/// and a printed error chain says it once.
impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<InvalidEvent> for StoreError {
    fn from(error: InvalidEvent) -> Self {
        Self::InvalidEvent(error)
    }
}

/// Why [`Store::open`] failed.
#[derive(Debug)]
pub enum StoreOpenError {
    Open(OpenError),
    Migrate(MigrateError),
    /// Reading the capture setting failed.
    Sqlite(rusqlite::Error),
    /// The writer thread could not be started.
    SpawnWriter(io::Error),
}

impl fmt::Display for StoreOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => error.fmt(f),
            Self::Migrate(error) => error.fmt(f),
            Self::Sqlite(error) => write!(f, "cannot read the store settings: {error}"),
            Self::SpawnWriter(error) => write!(f, "cannot start the writer thread: {error}"),
        }
    }
}

/// The message already includes the underlying error.
impl std::error::Error for StoreOpenError {}

impl From<OpenError> for StoreOpenError {
    fn from(error: OpenError) -> Self {
        Self::Open(error)
    }
}

impl From<MigrateError> for StoreOpenError {
    fn from(error: MigrateError) -> Self {
        Self::Migrate(error)
    }
}

/// The SQLite store: one writer thread, one reader, the read gate and the
/// capture setting. See the module docs.
pub struct Store {
    /// `None` once closed. Callers clone it for one send only.
    commands: Mutex<Option<mpsc::Sender<Command>>>,
    /// Becomes `true` once the writer thread has carried out every queued
    /// command and closed its connection. Closed without `true`: it panicked.
    writer_stopped: watch::Receiver<bool>,
    reader: Arc<Mutex<Connection>>,
    gate: Arc<RwLock<()>>,
    secrets: Arc<dyn SecretStore>,
    /// Published by the writer thread whenever it changes capture.
    capture: Arc<CaptureFlags>,
    /// Held for every capture change and key load, which serializes them.
    capture_changes: tokio::sync::Mutex<KeyLoad>,
    migrated: Migrated,
}

/// The writer's capture state, published for the async side.
///
/// Only the writer thread stores to these, right after it has changed its own
/// state, so they stay true even if a caller stops waiting for the reply.
#[derive(Debug)]
struct CaptureFlags {
    /// Capture is on.
    on: AtomicBool,
    /// The writer holds the content key (only ever while capture is on).
    key_loaded: AtomicBool,
}

/// The last failed attempt to load the key for an append.
#[derive(Debug, Default)]
struct KeyLoad {
    last_failure: Option<(Instant, KeychainUnavailable)>,
}

impl CaptureFlags {
    fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }

    fn key_loaded(&self) -> bool {
        self.key_loaded.load(Ordering::SeqCst)
    }
}

impl Store {
    /// Opens (creating if needed) and migrates the database in `data_dir`, opens
    /// the reader and starts the writer thread. Blocks; see the module docs.
    pub fn open(data_dir: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self, StoreOpenError> {
        let mut writer = db::open_writer(data_dir)?;
        let migrated = migrate::migrate(&mut writer, data_dir)?;
        let reader = db::open_reader(data_dir)?;
        let capture = read_capture_setting(&writer).map_err(StoreOpenError::Sqlite)?;

        let flags = Arc::new(CaptureFlags {
            on: AtomicBool::new(capture),
            key_loaded: AtomicBool::new(false),
        });
        let (commands, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (stopped, writer_stopped) = watch::channel(false);
        let state = WriterState {
            conn: writer,
            key: None,
            flags: Arc::clone(&flags),
        };
        // Detached: `close` waits for it through `writer_stopped`.
        thread::Builder::new()
            .name(WRITER_THREAD_NAME.to_owned())
            .spawn(move || state.run(receiver, stopped))
            .map_err(StoreOpenError::SpawnWriter)?;

        Ok(Self {
            commands: Mutex::new(Some(commands)),
            writer_stopped,
            reader: Arc::new(Mutex::new(reader)),
            gate: Arc::new(RwLock::new(())),
            secrets,
            capture: flags,
            capture_changes: tokio::sync::Mutex::new(KeyLoad::default()),
            migrated,
        })
    }

    /// What migrating did when this store was opened.
    pub fn migrated(&self) -> &Migrated {
        &self.migrated
    }

    /// Appends one event to its run's chain. See the module docs.
    pub async fn append(&self, event: AppendEvent) -> Result<AppendedEvent, StoreError> {
        let event = ValidEvent::new(event)?;
        // The writer asks for the key only after opening with capture already
        // on (see the module docs); the key is then loaded and the event resent
        // once.
        let event = match self.send_append(event).await? {
            Err(AppendFailure::NeedsKey(event)) => {
                self.load_key().await?;
                event
            }
            outcome => return settle(outcome),
        };
        match self.send_append(event).await? {
            Err(AppendFailure::NeedsKey(_)) => Err(StoreError::Keychain(KeychainUnavailable::new(
                "The content key was not loaded, so the event was not written.",
            ))),
            outcome => settle(outcome),
        }
    }

    /// Whether content capture is on (R3.4; off unless enabled).
    pub fn capture_content(&self) -> bool {
        self.capture.is_on()
    }

    /// Turns content capture on or off and returns the new setting.
    ///
    /// Enabling gets the content key first, even within
    /// [`KEY_RETRY_INTERVAL`] of a failed attempt. If it can't, it fails with
    /// [`StoreError::Keychain`] and capture is off afterwards: a setting left
    /// on from an earlier run, whose key could not be loaded, is turned off
    /// too, so the setting agrees with the error. See the module docs.
    pub async fn set_capture_content(&self, enabled: bool) -> Result<bool, StoreError> {
        let mut changing = self.capture_changes.lock().await;
        changing.last_failure = None;
        if !enabled {
            self.set_writer_capture(None).await?;
        } else if !self.capture.key_loaded() {
            match self.obtain_key().await {
                Ok(key) => self.set_writer_capture(Some(key)).await?,
                Err(error @ StoreError::Keychain(_)) => {
                    if self.capture.is_on() {
                        self.set_writer_capture(None).await?;
                    }
                    return Err(error);
                }
                Err(other) => return Err(other),
            }
        }
        Ok(enabled)
    }

    /// Runs `f` in one read transaction on the read-only connection, holding
    /// the read gate shared until it ends.
    ///
    /// `f` runs on a blocking thread; reads run one at a time. The gate is held
    /// by that thread, so a caller that stops waiting doesn't release it early.
    pub async fn read<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let gate = Arc::clone(&self.gate).read_owned().await;
        let reader = Arc::clone(&self.reader);
        let result = tokio::task::spawn_blocking(move || {
            let _gate = gate;
            let mut conn = reader.lock().unwrap_or_else(PoisonError::into_inner);
            let tx = conn.transaction()?;
            let value = f(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
        .map_err(|_| StoreError::TaskFailed)?;
        result.map_err(StoreError::Sqlite)
    }

    /// Takes the read gate exclusively: waits for every open read to finish and
    /// holds new reads back until the guard is dropped. For erasure (task 10).
    pub async fn exclude_readers(&self) -> OwnedRwLockWriteGuard<()> {
        Arc::clone(&self.gate).write_owned().await
    }

    /// Global position of the newest event; 0 for an empty log.
    pub async fn last_global_position(&self) -> Result<u64, StoreError> {
        self.read(last_global_position).await
    }

    /// How many content blobs are stored.
    pub async fn blob_count(&self) -> Result<u64, StoreError> {
        self.read(content::blob_count).await
    }

    /// Closes the command queue, lets the writer finish every queued command,
    /// and waits for its thread to end. Later writes fail with
    /// [`StoreError::Closed`]; reads still work.
    ///
    /// Every call waits, including concurrent and repeated ones. Fails with
    /// [`StoreError::WriterFailed`] if the writer thread panicked.
    pub async fn close(&self) -> Result<(), StoreError> {
        drop(
            self.commands
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let mut stopped = self.writer_stopped.clone();
        match stopped.wait_for(|stopped| *stopped).await {
            Ok(_) => Ok(()),
            Err(_) => Err(StoreError::WriterFailed),
        }
    }

    /// Sends one command and waits for its reply.
    async fn request<R>(
        &self,
        command: impl FnOnce(oneshot::Sender<R>) -> Command,
    ) -> Result<R, StoreError> {
        let sender = self
            .commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(StoreError::Closed)?;
        let (reply, response) = oneshot::channel();
        let sent = sender.send(command(reply)).await;
        // Only the queue itself may keep the writer running once closed.
        drop(sender);
        if sent.is_err() {
            return Err(self.stopped_error());
        }
        response.await.map_err(|_| self.stopped_error())
    }

    /// The writer stopped answering: closed if [`Store::close`] closed the
    /// queue (a queued command is always answered then), failed otherwise.
    fn stopped_error(&self) -> StoreError {
        let closed = self
            .commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none();
        if closed {
            StoreError::Closed
        } else {
            StoreError::WriterFailed
        }
    }

    async fn send_append(
        &self,
        event: ValidEvent,
    ) -> Result<Result<AppendedEvent, AppendFailure>, StoreError> {
        self.request(|reply| Command::Append { event, reply }).await
    }

    async fn set_writer_capture(&self, key: Option<ContentKey>) -> Result<(), StoreError> {
        self.request(|reply| Command::SetCapture { key, reply })
            .await?
            .map_err(StoreError::Sqlite)
    }

    /// Loads the key into the writer when capture is on but the key isn't
    /// loaded yet (after opening with capture on).
    ///
    /// Within [`KEY_RETRY_INTERVAL`] of a failed attempt it fails at once with
    /// that attempt's error, so a burst of appends asks the keychain (and may
    /// prompt the user) once, not once per append.
    async fn load_key(&self) -> Result<(), StoreError> {
        let mut changing = self.capture_changes.lock().await;
        if self.capture.key_loaded() || !self.capture.is_on() {
            return Ok(());
        }
        if let Some((at, error)) = &changing.last_failure
            && at.elapsed() < KEY_RETRY_INTERVAL
        {
            return Err(StoreError::Keychain(error.clone()));
        }
        match self.obtain_key().await {
            Ok(key) => {
                changing.last_failure = None;
                self.set_writer_capture(Some(key)).await
            }
            Err(StoreError::Keychain(error)) => {
                changing.last_failure = Some((Instant::now(), error.clone()));
                Err(StoreError::Keychain(error))
            }
            Err(other) => Err(other),
        }
    }

    /// Gets the content key on a blocking thread. It may be generated only
    /// while no blob is stored. Call with `capture_changes` locked.
    async fn obtain_key(&self) -> Result<ContentKey, StoreError> {
        let if_missing = if self.read(content::has_blobs).await? {
            IfMissing::Fail
        } else {
            IfMissing::Generate
        };
        let secrets = Arc::clone(&self.secrets);
        let mut bytes = tokio::task::spawn_blocking(move || secrets.content_key(if_missing))
            .await
            .map_err(|_| StoreError::TaskFailed)?
            .map_err(StoreError::Keychain)?;
        Ok(ContentKey::take(&mut bytes))
    }
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("capture_content", &self.capture_content())
            .field("migrated", &self.migrated)
            .finish_non_exhaustive()
    }
}

/// An event that passed every check that doesn't need the database. `body` is
/// a JSON object.
struct ValidEvent {
    run_id: String,
    kind: String,
    ts_ms: u64,
    body: Value,
    content: Vec<Vec<u8>>,
}

impl ValidEvent {
    fn new(event: AppendEvent) -> Result<Self, InvalidEvent> {
        check_name(
            &event.run_id,
            InvalidEvent::EmptyRunId,
            InvalidEvent::RunIdTooLong,
        )?;
        check_name(
            &event.kind,
            InvalidEvent::EmptyKind,
            InvalidEvent::KindTooLong,
        )?;
        if event.ts_ms > MAX_SAFE_INTEGER {
            return Err(InvalidEvent::TimestampOutOfRange);
        }
        let Some(members) = event.body.as_object() else {
            return Err(InvalidEvent::BodyNotAnObject);
        };
        if members.contains_key(CONTENT_FIELD) {
            return Err(InvalidEvent::BodyHasContentField);
        }
        event::check_body_numbers(&event.body).map_err(InvalidEvent::Body)?;
        Ok(Self {
            run_id: event.run_id,
            kind: event.kind,
            ts_ms: event.ts_ms,
            body: event.body,
            content: event.content,
        })
    }
}

/// The result of an append that didn't need the key.
fn settle(outcome: Result<AppendedEvent, AppendFailure>) -> Result<AppendedEvent, StoreError> {
    match outcome {
        Ok(appended) => Ok(appended),
        Err(AppendFailure::Error(error)) => Err(error),
        Err(AppendFailure::NeedsKey(_)) => Err(StoreError::Keychain(KeychainUnavailable::new(
            "The content key was not loaded, so the event was not written.",
        ))),
    }
}

fn check_name(name: &str, empty: InvalidEvent, too_long: InvalidEvent) -> Result<(), InvalidEvent> {
    if name.is_empty() {
        Err(empty)
    } else if name.len() > MAX_NAME_LEN {
        Err(too_long)
    } else {
        Ok(())
    }
}

enum Command {
    Append {
        event: ValidEvent,
        reply: oneshot::Sender<Result<AppendedEvent, AppendFailure>>,
    },
    /// `Some(key)`: turn capture on and keep the key. `None`: turn it off and
    /// drop the key.
    SetCapture {
        key: Option<ContentKey>,
        reply: oneshot::Sender<rusqlite::Result<()>>,
    },
}

enum AppendFailure {
    /// Capture is on and the event has content, but the writer has no key yet.
    /// The event comes back so the caller can load the key and resend it.
    NeedsKey(ValidEvent),
    Error(StoreError),
}

/// Everything the writer thread owns.
struct WriterState {
    conn: Connection,
    /// `Some` only while capture is on.
    key: Option<ContentKey>,
    flags: Arc<CaptureFlags>,
}

impl WriterState {
    /// Carries out commands until every sender is gone and the queue is empty,
    /// closes the connection, then reports `true` on `stopped`. A panic drops
    /// `stopped` without that report.
    fn run(mut self, mut commands: mpsc::Receiver<Command>, stopped: watch::Sender<bool>) {
        let _report = ReportPanic;
        while let Some(command) = commands.blocking_recv() {
            // A caller that stopped waiting has dropped its receiver; the command
            // was still carried out.
            match command {
                Command::Append { event, reply } => {
                    let _ = reply.send(self.append(event));
                }
                Command::SetCapture { key, reply } => {
                    let _ = reply.send(self.set_capture(key));
                }
            }
        }
        let Self { conn, key, .. } = self;
        drop(key);
        if let Err((_, error)) = conn.close() {
            tracing::warn!(%error, "closing the writer connection failed");
        }
        stopped.send_replace(true);
    }

    fn append(&mut self, event: ValidEvent) -> Result<AppendedEvent, AppendFailure> {
        let key = if self.flags.is_on() && !event.content.is_empty() {
            match &self.key {
                Some(key) => Some(key),
                None => return Err(AppendFailure::NeedsKey(event)),
            }
        } else {
            None
        };
        append_in_transaction(&mut self.conn, event, key).map_err(AppendFailure::Error)
    }

    fn set_capture(&mut self, key: Option<ContentKey>) -> rusqlite::Result<()> {
        let enabled = key.is_some();
        write_capture_setting(&self.conn, enabled)?;
        // Replacing the old key drops it, which zeroes it.
        self.key = key;
        self.flags.on.store(enabled, Ordering::SeqCst);
        self.flags.key_loaded.store(enabled, Ordering::SeqCst);
        Ok(())
    }
}

/// Logs a panic of the writer thread, which otherwise only shows as failed
/// writes.
struct ReportPanic;

impl Drop for ReportPanic {
    fn drop(&mut self) {
        if thread::panicking() {
            tracing::error!("the store writer thread panicked; nothing more can be written");
        }
    }
}

/// Appends `event` in its own `BEGIN IMMEDIATE` transaction. `key` is the
/// content key when content is to be stored, `None` to ignore the content.
fn append_in_transaction(
    conn: &mut Connection,
    event: ValidEvent,
    key: Option<&ContentKey>,
) -> Result<AppendedEvent, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let appended = append_in(&tx, event, key)?;
    tx.commit()?;
    Ok(appended)
}

/// The append itself, inside the caller's transaction. Kept apart from the
/// transaction so erasure (task 10) can append its `content.erased` event in
/// the same transaction as its other writes.
fn append_in(
    conn: &Connection,
    event: ValidEvent,
    key: Option<&ContentKey>,
) -> Result<AppendedEvent, StoreError> {
    let ValidEvent {
        run_id,
        kind,
        ts_ms,
        mut body,
        content,
    } = event;

    let last = conn
        .prepare_cached("SELECT last_seq, last_hash FROM runs WHERE run_id = ?1")?
        .query_row([&run_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .optional()?;
    let (seq, prev_hash) = match &last {
        None => (1, ZERO_HASH.to_owned()),
        Some((last_seq, last_hash)) => {
            let last_seq = u64::try_from(*last_seq)
                .ok()
                .filter(|last_seq| *last_seq >= 1)
                .ok_or(StoreError::Chain("a run's last_seq is below 1"))?;
            (next_position(last_seq)?, last_hash.clone())
        }
    };
    let global_pos = next_position(last_global_position(conn)?)?;
    check_link(seq, &prev_hash)?;

    if last.is_none() {
        conn.prepare_cached(
            "INSERT INTO runs (run_id, created_ms, last_seq, last_hash) VALUES (?1, ?2, 0, ?3)",
        )?
        .execute(params![run_id, to_sql_int(ts_ms)?, ZERO_HASH])?;
    }

    let addresses = match key {
        Some(key) if !content.is_empty() => {
            let mut addresses = Vec::with_capacity(content.len());
            for bytes in &content {
                addresses.push(content::put_content(conn, key, bytes)?);
            }
            addresses
        }
        _ => Vec::new(),
    };
    if let (false, Value::Object(members)) = (addresses.is_empty(), &mut body) {
        let listed = addresses.iter().cloned().map(Value::String).collect();
        members.insert(CONTENT_FIELD.to_owned(), Value::Array(listed));
    }

    let event_hash = event::event_hash(&HashedEvent {
        run_id: &run_id,
        seq,
        global_pos,
        kind: &kind,
        ts_ms,
        body: &body,
        prev_hash: &prev_hash,
    })
    .map_err(hash_error)?;
    let canonical = event::canonical_json(&body)
        .map_err(|error| StoreError::Hash(EventHashError::Canonicalization(error)))?;
    let canonical = String::from_utf8(canonical)
        .map_err(|_| StoreError::Chain("the canonical body is not UTF-8"))?;

    conn.prepare_cached(
        "INSERT INTO events (global_pos, run_id, seq, kind, ts_ms, body, prev_hash, event_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        to_sql_int(global_pos)?,
        run_id,
        to_sql_int(seq)?,
        kind,
        to_sql_int(ts_ms)?,
        canonical,
        prev_hash,
        event_hash,
    ])?;
    let mut link = conn.prepare_cached(
        "INSERT OR IGNORE INTO event_content (global_pos, address) VALUES (?1, ?2)",
    )?;
    for address in &addresses {
        link.execute(params![to_sql_int(global_pos)?, address])?;
    }
    conn.prepare_cached("UPDATE runs SET last_seq = ?1, last_hash = ?2 WHERE run_id = ?3")?
        .execute(params![to_sql_int(seq)?, event_hash, run_id])?;

    Ok(AppendedEvent {
        run_id,
        seq,
        global_pos,
        event_hash,
    })
}

/// The position after `last`, which stays within 2^53 − 1.
fn next_position(last: u64) -> Result<u64, StoreError> {
    last.checked_add(1)
        .filter(|next| *next <= MAX_SAFE_INTEGER)
        .ok_or(StoreError::Chain("a position would pass 2^53 − 1"))
}

/// Checks the link before hashing: `seq` from 1, `prev_hash` 64 lowercase hex
/// characters, and `ZERO_HASH` exactly for `seq = 1`.
fn check_link(seq: u64, prev_hash: &str) -> Result<(), StoreError> {
    if seq == 0 {
        return Err(StoreError::Chain("seq must start at 1"));
    }
    let is_hash = prev_hash.len() == 64
        && prev_hash
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !is_hash {
        return Err(StoreError::Chain(
            "a run's last hash is not 64 lowercase hex characters",
        ));
    }
    if (seq == 1) != (prev_hash == ZERO_HASH) {
        return Err(StoreError::Chain(
            "prev_hash must be the zero hash for seq 1 and only then",
        ));
    }
    Ok(())
}

fn hash_error(error: EventHashError) -> StoreError {
    match error {
        EventHashError::Body(body) => StoreError::InvalidEvent(InvalidEvent::Body(body)),
        EventHashError::FieldOutOfRange(_) => StoreError::Chain("a position would pass 2^53 − 1"),
        other => StoreError::Hash(other),
    }
}

/// Every position and timestamp is at most 2^53 − 1, so this never fails in
/// practice; it keeps the conversion checked.
fn to_sql_int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Chain("a number is too large for SQLite"))
}

/// Global position of the newest event; 0 for an empty log.
fn last_global_position(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row(
        "SELECT coalesce(max(global_pos), 0) FROM events",
        [],
        |row| {
            let last: i64 = row.get(0)?;
            u64::try_from(last).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, last))
        },
    )
}

/// Absent (or anything but `"true"`) reads as off (R3.4).
fn read_capture_setting(conn: &Connection) -> rusqlite::Result<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [CAPTURE_CONTENT_SETTING],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.as_deref() == Some("true"))
}

fn write_capture_setting(conn: &Connection, enabled: bool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![
            CAPTURE_CONTENT_SETTING,
            if enabled { "true" } else { "false" }
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::secrets::{InMemorySecretStore, KEY_LEN};

    const KEY: [u8; KEY_LEN] = [42; KEY_LEN];
    const TS: u64 = 1_790_000_000_000;

    fn in_memory() -> Arc<dyn SecretStore> {
        Arc::new(InMemorySecretStore::new(KEY))
    }

    fn open(dir: &Path) -> Store {
        Store::open(dir, in_memory()).unwrap()
    }

    fn event(run: &str, n: u64) -> AppendEvent {
        AppendEvent {
            run_id: run.to_owned(),
            kind: "test.event".to_owned(),
            ts_ms: TS + n,
            body: json!({ "n": n }),
            content: Vec::new(),
        }
    }

    fn with_content(run: &str, n: u64, items: &[&[u8]]) -> AppendEvent {
        AppendEvent {
            content: items.iter().map(|item| item.to_vec()).collect(),
            ..event(run, n)
        }
    }

    fn address(bytes: &[u8]) -> String {
        content::content_address(&ContentKey::take(&mut { KEY }), bytes)
    }

    /// Records every `if_missing` it is asked with.
    struct RecordingSecretStore {
        calls: Mutex<Vec<IfMissing>>,
        available: bool,
    }

    impl RecordingSecretStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                available: true,
            })
        }

        fn unavailable() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                available: false,
            })
        }

        fn calls(&self) -> Vec<IfMissing> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl SecretStore for RecordingSecretStore {
        fn content_key(&self, if_missing: IfMissing) -> Result<[u8; KEY_LEN], KeychainUnavailable> {
            self.calls.lock().unwrap().push(if_missing);
            if self.available {
                Ok(KEY)
            } else {
                Err(KeychainUnavailable::new("keychain locked"))
            }
        }
    }

    #[derive(Debug)]
    struct Row {
        global_pos: i64,
        run_id: String,
        seq: i64,
        kind: String,
        ts_ms: i64,
        body: String,
        prev_hash: String,
        event_hash: String,
    }

    async fn rows(store: &Store) -> Vec<Row> {
        store
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT global_pos, run_id, seq, kind, ts_ms, body, prev_hash, event_hash
                     FROM events ORDER BY global_pos",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(Row {
                            global_pos: row.get(0)?,
                            run_id: row.get(1)?,
                            seq: row.get(2)?,
                            kind: row.get(3)?,
                            ts_ms: row.get(4)?,
                            body: row.get(5)?,
                            prev_hash: row.get(6)?,
                            event_hash: row.get(7)?,
                        })
                    })?
                    .collect()
            })
            .await
            .unwrap()
    }

    async fn runs(store: &Store) -> HashMap<String, (i64, String)> {
        store
            .read(|conn| {
                let mut statement = conn.prepare("SELECT run_id, last_seq, last_hash FROM runs")?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?
                    .collect()
            })
            .await
            .unwrap()
    }

    async fn event_content(store: &Store) -> Vec<(i64, String)> {
        store
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT global_pos, address FROM event_content ORDER BY global_pos, address",
                )?;
                statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .await
            .unwrap()
    }

    async fn setting(store: &Store) -> Option<String> {
        store
            .read(|conn| {
                conn.query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    [CAPTURE_CONTENT_SETTING],
                    |row| row.get(0),
                )
                .optional()
            })
            .await
            .unwrap()
    }

    fn body_of(row: &Row) -> Value {
        serde_json::from_str(&row.body).unwrap()
    }

    /// Recomputes every hash and every link from the stored rows, and checks
    /// `runs` against the last event of each run.
    async fn assert_chain(store: &Store) -> usize {
        let rows = rows(store).await;
        let mut last: HashMap<String, (i64, String)> = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row.global_pos, index as i64 + 1, "global_pos gap");
            let (expected_seq, expected_prev) = match last.get(&row.run_id) {
                None => (1, ZERO_HASH.to_owned()),
                Some((seq, hash)) => (seq + 1, hash.clone()),
            };
            assert_eq!(row.seq, expected_seq, "seq gap in {}", row.run_id);
            assert_eq!(
                row.prev_hash, expected_prev,
                "prev link at {}",
                row.global_pos
            );

            let body = body_of(row);
            let canonical = event::canonical_json(&body).unwrap();
            assert_eq!(row.body.as_bytes(), canonical, "stored body is canonical");
            let recomputed = event::event_hash(&HashedEvent {
                run_id: &row.run_id,
                seq: row.seq as u64,
                global_pos: row.global_pos as u64,
                kind: &row.kind,
                ts_ms: row.ts_ms as u64,
                body: &body,
                prev_hash: &row.prev_hash,
            })
            .unwrap();
            assert_eq!(row.event_hash, recomputed, "hash at {}", row.global_pos);
            last.insert(row.run_id.clone(), (row.seq, row.event_hash.clone()));
        }
        assert_eq!(runs(store).await, last);
        rows.len()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn eight_concurrent_writers_leave_gap_free_verifiable_chains() {
        const WRITERS: u64 = 8;
        const EACH: u64 = 1_000;
        const RUNS: u64 = 4;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(open(dir.path()));
        let started = std::time::Instant::now();

        let tasks: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let store = Arc::clone(&store);
                tokio::spawn(async move {
                    let mut appended = Vec::new();
                    for i in 0..EACH {
                        // Every writer spreads over every run, so runs are
                        // appended to concurrently.
                        let run = format!("run-{}", (writer + i) % RUNS);
                        appended.push(store.append(event(&run, writer * EACH + i)).await.unwrap());
                    }
                    appended
                })
            })
            .collect();
        let mut appended = Vec::new();
        for task in tasks {
            appended.extend(task.await.unwrap());
        }
        let elapsed = started.elapsed();

        let total = WRITERS * EACH;
        let positions: HashSet<u64> = appended.iter().map(|a| a.global_pos).collect();
        assert_eq!(positions, (1..=total).collect());
        let mut per_run: HashMap<&str, Vec<u64>> = HashMap::new();
        for a in &appended {
            per_run.entry(&a.run_id).or_default().push(a.seq);
        }
        for seqs in per_run.values_mut() {
            seqs.sort_unstable();
            assert_eq!(*seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
        }
        assert_eq!(assert_chain(&store).await, total as usize);
        assert_eq!(store.last_global_position().await.unwrap(), total);
        println!(
            "{WRITERS} writers x {EACH} events over {RUNS} runs: global_pos 1..={total} gap-free, \
             every seq gap-free, every hash and prev link recomputed, in {elapsed:?}"
        );
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_run_s_first_event_links_to_the_zero_hash() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        assert_eq!(store.last_global_position().await.unwrap(), 0);

        let first = store.append(event("run-a", 0)).await.unwrap();
        let second = store.append(event("run-a", 1)).await.unwrap();
        let other = store.append(event("run-b", 2)).await.unwrap();

        assert_eq!((first.seq, first.global_pos), (1, 1));
        assert_eq!((second.seq, second.global_pos), (2, 2));
        assert_eq!((other.seq, other.global_pos), (1, 3));
        let rows = rows(&store).await;
        assert_eq!(rows[0].prev_hash, ZERO_HASH);
        assert_eq!(rows[1].prev_hash, first.event_hash);
        assert_eq!(rows[2].prev_hash, ZERO_HASH);
        assert_eq!(rows[0].body, r#"{"n":0}"#);
        assert_eq!(assert_chain(&store).await, 3);
        let created: i64 = store
            .read(|conn| {
                conn.query_row(
                    "SELECT created_ms FROM runs WHERE run_id = 'run-b'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(created, (TS + 2) as i64);
    }

    #[tokio::test]
    async fn floats_and_unsafe_integers_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let too_big = MAX_SAFE_INTEGER + 1;

        for body in [
            json!({ "cost": 1.5 }),
            json!({ "n": too_big }),
            json!({ "n": -(too_big as i64) }),
            json!({ "nested": [{ "n": u64::MAX }] }),
        ] {
            let error = store
                .append(AppendEvent {
                    body,
                    ..event("run-a", 0)
                })
                .await
                .unwrap_err();
            assert!(
                matches!(error, StoreError::InvalidEvent(InvalidEvent::Body(_))),
                "{error:?}"
            );
        }

        let at_limit = json!({ "max": MAX_SAFE_INTEGER, "min": -(MAX_SAFE_INTEGER as i64) });
        store
            .append(AppendEvent {
                body: at_limit,
                ..event("run-a", 0)
            })
            .await
            .unwrap();
        assert_eq!(store.last_global_position().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn malformed_events_are_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let long = "x".repeat(MAX_NAME_LEN + 1);
        let cases = [
            (
                AppendEvent {
                    run_id: String::new(),
                    ..event("r", 0)
                },
                InvalidEvent::EmptyRunId,
            ),
            (
                AppendEvent {
                    run_id: long.clone(),
                    ..event("r", 0)
                },
                InvalidEvent::RunIdTooLong,
            ),
            (
                AppendEvent {
                    kind: String::new(),
                    ..event("r", 0)
                },
                InvalidEvent::EmptyKind,
            ),
            (
                AppendEvent {
                    kind: long,
                    ..event("r", 0)
                },
                InvalidEvent::KindTooLong,
            ),
            (
                AppendEvent {
                    ts_ms: MAX_SAFE_INTEGER + 1,
                    ..event("r", 0)
                },
                InvalidEvent::TimestampOutOfRange,
            ),
            (
                AppendEvent {
                    body: json!([1, 2]),
                    ..event("r", 0)
                },
                InvalidEvent::BodyNotAnObject,
            ),
            (
                AppendEvent {
                    body: json!({ "content": ["0".repeat(64)] }),
                    ..event("r", 0)
                },
                InvalidEvent::BodyHasContentField,
            ),
        ];

        for (bad, expected) in cases {
            match store.append(bad).await {
                Err(StoreError::InvalidEvent(found)) => assert_eq!(found, expected),
                other => panic!("expected {expected:?}, got {other:?}"),
            }
        }

        // A nested `content` member is ordinary metadata.
        store
            .append(AppendEvent {
                body: json!({ "call": { "content": 1 } }),
                ..event("r", 0)
            })
            .await
            .unwrap();
        assert_eq!(store.last_global_position().await.unwrap(), 1);
        assert_eq!(runs(&store).await.len(), 1);
    }

    #[tokio::test]
    async fn capture_is_off_by_default_and_ignores_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());

        assert!(!store.capture_content());
        assert_eq!(setting(&store).await, None);
        store
            .append(with_content("run-a", 1, &[b"secret prompt", b"reply"]))
            .await
            .unwrap();

        let rows = rows(&store).await;
        assert_eq!(rows[0].body, r#"{"n":1}"#);
        assert_eq!(store.blob_count().await.unwrap(), 0);
        assert!(event_content(&store).await.is_empty());
        assert_eq!(assert_chain(&store).await, 1);
    }

    #[tokio::test]
    async fn capture_on_stores_identical_content_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        assert!(store.set_capture_content(true).await.unwrap());
        assert!(store.capture_content());
        assert_eq!(setting(&store).await.as_deref(), Some("true"));
        let (shared, other) = (
            b"the same system prompt".as_slice(),
            b"an answer".as_slice(),
        );

        let a = store
            .append(with_content("run-a", 1, &[shared, other]))
            .await
            .unwrap();
        let b = store
            .append(with_content("run-b", 2, &[shared]))
            .await
            .unwrap();
        let c = store
            .append(with_content("run-a", 3, &[shared, shared]))
            .await
            .unwrap();
        let none = store.append(event("run-a", 4)).await.unwrap();

        assert_eq!(store.blob_count().await.unwrap(), 2);
        let rows = rows(&store).await;
        let listed = |row: &Row| body_of(row)[CONTENT_FIELD].clone();
        assert_eq!(listed(&rows[0]), json!([address(shared), address(other)]));
        assert_eq!(listed(&rows[1]), json!([address(shared)]));
        assert_eq!(listed(&rows[2]), json!([address(shared), address(shared)]));
        assert_eq!(body_of(&rows[3]), json!({ "n": 4 }));
        let mut expected_links = vec![
            (a.global_pos as i64, address(shared)),
            (a.global_pos as i64, address(other)),
            (b.global_pos as i64, address(shared)),
            (c.global_pos as i64, address(shared)),
        ];
        expected_links.sort();
        assert_eq!(event_content(&store).await, expected_links);
        assert!(
            event_content(&store)
                .await
                .iter()
                .all(|(pos, _)| *pos != none.global_pos as i64)
        );
        let stored: Vec<u8> = store
            .read(move |conn| {
                conn.query_row(
                    "SELECT bytes FROM blobs WHERE address = ?1",
                    [address(b"the same system prompt")],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(stored, shared);
        assert_eq!(assert_chain(&store).await, 4);
        println!(
            "dedup: 3 events in 2 runs carried 5 content items (2 distinct); blobs stored: {}",
            store.blob_count().await.unwrap()
        );
    }

    #[tokio::test]
    async fn disabling_capture_stops_storing_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        store.set_capture_content(true).await.unwrap();

        assert!(!store.set_capture_content(false).await.unwrap());
        store
            .append(with_content("run-a", 1, &[b"after disabling"]))
            .await
            .unwrap();

        assert!(!store.capture_content());
        assert_eq!(setting(&store).await.as_deref(), Some("false"));
        assert_eq!(store.blob_count().await.unwrap(), 0);
        assert_eq!(rows(&store).await[0].body, r#"{"n":1}"#);
    }

    #[tokio::test]
    async fn enabling_without_a_keychain_fails_with_1001_and_capture_stays_off() {
        let dir = tempfile::tempdir().unwrap();
        let unavailable: Arc<dyn SecretStore> =
            Arc::new(InMemorySecretStore::unavailable("no keychain here"));
        let store = Store::open(dir.path(), unavailable).unwrap();

        let error = store.set_capture_content(true).await.unwrap_err();

        let StoreError::Keychain(keychain) = &error else {
            panic!("expected a keychain error, got {error:?}");
        };
        assert_eq!(keychain.code(), 1001);
        assert_eq!(error.to_string(), "no keychain here");
        assert!(!store.capture_content());
        assert_eq!(setting(&store).await, None);
        store
            .append(with_content("run-a", 1, &[b"not stored"]))
            .await
            .unwrap();
        assert_eq!(store.blob_count().await.unwrap(), 0);
        store.close().await.unwrap();

        assert!(!open(dir.path()).capture_content());
    }

    #[tokio::test]
    async fn the_key_may_be_generated_only_while_no_blob_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let recording = RecordingSecretStore::new();
        let store = Store::open(dir.path(), recording.clone()).unwrap();

        store.set_capture_content(true).await.unwrap();
        // Already on with the key loaded: no second keychain call.
        store.set_capture_content(true).await.unwrap();
        store
            .append(with_content("run-a", 1, &[b"stored"]))
            .await
            .unwrap();
        store.set_capture_content(false).await.unwrap();
        store.set_capture_content(true).await.unwrap();

        assert_eq!(recording.calls(), [IfMissing::Generate, IfMissing::Fail]);
    }

    #[tokio::test]
    async fn the_capture_setting_persists_across_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        store.set_capture_content(true).await.unwrap();
        store.close().await.unwrap();
        drop(store);

        let store = open(dir.path());
        assert!(store.capture_content());
        store.set_capture_content(false).await.unwrap();
        store.close().await.unwrap();
        drop(store);

        assert!(!open(dir.path()).capture_content());
    }

    #[tokio::test]
    async fn reopening_with_capture_on_loads_the_key_on_the_first_content_append() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        store.set_capture_content(true).await.unwrap();
        store
            .append(with_content("run-a", 1, &[b"before reopening"]))
            .await
            .unwrap();
        store.close().await.unwrap();
        drop(store);

        let recording = RecordingSecretStore::new();
        let store = Store::open(dir.path(), recording.clone()).unwrap();
        store.append(event("run-a", 2)).await.unwrap();
        assert!(
            recording.calls().is_empty(),
            "open and plain appends need no key"
        );
        store
            .append(with_content("run-b", 3, &[b"before reopening", b"after"]))
            .await
            .unwrap();
        store
            .append(with_content("run-b", 4, &[b"after"]))
            .await
            .unwrap();

        // Blobs exist, so a missing key must not be replaced.
        assert_eq!(recording.calls(), [IfMissing::Fail]);
        assert_eq!(store.blob_count().await.unwrap(), 2);
        assert_eq!(
            body_of(&rows(&store).await[2])[CONTENT_FIELD],
            json!([address(b"before reopening"), address(b"after")])
        );
        assert_eq!(assert_chain(&store).await, 4);
    }

    async fn store_with_capture_left_on(dir: &Path) {
        let store = open(dir);
        store.set_capture_content(true).await.unwrap();
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn reopening_with_capture_on_and_no_keychain_refuses_content_appends() {
        let dir = tempfile::tempdir().unwrap();
        store_with_capture_left_on(dir.path()).await;
        let locked = RecordingSecretStore::unavailable();
        let store = Store::open(dir.path(), locked.clone()).unwrap();

        let error = store
            .append(with_content("run-a", 1, &[b"must not be dropped silently"]))
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::Keychain(_)), "{error:?}");
        // Within the retry interval the next one fails without asking again.
        let again = store
            .append(with_content("run-a", 2, &[b"nor this"]))
            .await
            .unwrap_err();
        assert_eq!(again.to_string(), "keychain locked");
        assert_eq!(locked.calls(), [IfMissing::Generate]);

        assert_eq!(store.last_global_position().await.unwrap(), 0);
        store.append(event("run-a", 3)).await.unwrap();
        assert_eq!(store.last_global_position().await.unwrap(), 1);
        assert!(store.capture_content());
    }

    #[tokio::test]
    async fn enabling_again_without_a_key_turns_a_leftover_setting_off() {
        let dir = tempfile::tempdir().unwrap();
        store_with_capture_left_on(dir.path()).await;
        let locked = RecordingSecretStore::unavailable();
        let store = Store::open(dir.path(), locked.clone()).unwrap();
        assert!(store.capture_content());

        let error = store.set_capture_content(true).await.unwrap_err();

        assert!(matches!(error, StoreError::Keychain(_)), "{error:?}");
        assert!(!store.capture_content());
        assert_eq!(setting(&store).await.as_deref(), Some("false"));
        assert_eq!(locked.calls(), [IfMissing::Generate]);
    }

    #[tokio::test]
    async fn events_stay_append_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        store.append(event("run-a", 1)).await.unwrap();
        store.close().await.unwrap();
        drop(store);

        let conn = db::open_writer(dir.path()).unwrap();
        for sql in ["UPDATE events SET kind = 'edited'", "DELETE FROM events"] {
            let error = conn.execute(sql, []).unwrap_err();
            assert!(
                error.to_string().contains("events are append-only"),
                "{error}"
            );
        }
    }

    #[tokio::test]
    async fn close_carries_out_every_queued_append() {
        const QUEUED: u64 = 100;
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        // Queue the commands directly, all at once, so every one of them is
        // waiting in the queue when the store closes.
        let sender = store.commands.lock().unwrap().clone().unwrap();
        let replies: Vec<_> = (0..QUEUED)
            .map(|n| {
                let (reply, response) = oneshot::channel();
                let event = ValidEvent::new(event("run-a", n)).unwrap();
                assert!(sender.try_send(Command::Append { event, reply }).is_ok());
                response
            })
            .collect();
        drop(sender);

        store.close().await.unwrap();
        // Closing again waits for the same, finished thread.
        store.close().await.unwrap();

        for (n, response) in (1..).zip(replies) {
            let Ok(Ok(appended)) = response.await else {
                panic!("append {n} was not carried out");
            };
            assert_eq!((appended.seq, appended.global_pos), (n, n));
        }
        assert_eq!(store.last_global_position().await.unwrap(), QUEUED);
        assert!(matches!(
            store.append(event("run-a", 0)).await,
            Err(StoreError::Closed)
        ));
        assert!(matches!(
            store.set_capture_content(false).await,
            Err(StoreError::Closed)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn appends_racing_close_either_commit_or_report_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(open(dir.path()));
        let tasks: Vec<_> = (0..200)
            .map(|n| {
                let store = Arc::clone(&store);
                tokio::spawn(async move { store.append(event("run-a", n)).await })
            })
            .collect();

        // Two concurrent closes both wait for the writer to finish.
        let other_close = {
            let store = Arc::clone(&store);
            tokio::spawn(async move { store.close().await })
        };
        store.close().await.unwrap();
        other_close.await.unwrap().unwrap();

        let mut committed = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => committed += 1,
                Err(StoreError::Closed) => {}
                Err(other) => panic!("unexpected error {other:?}"),
            }
        }
        assert_eq!(store.last_global_position().await.unwrap(), committed);
        assert_eq!(assert_chain(&store).await as u64, committed);
    }

    #[tokio::test]
    async fn reads_wait_while_readers_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(open(dir.path()));
        store.append(event("run-a", 1)).await.unwrap();

        let exclusive = store.exclude_readers().await;
        let reader = {
            let store = Arc::clone(&store);
            tokio::spawn(async move { store.last_global_position().await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !reader.is_finished(),
            "a read ran while readers were excluded"
        );

        drop(exclusive);
        assert_eq!(reader.await.unwrap().unwrap(), 1);
    }

    #[tokio::test]
    async fn exclusion_waits_for_an_open_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(open(dir.path()));
        let (entered, wait_entered) = std::sync::mpsc::channel();
        let (release, wait_release) = std::sync::mpsc::channel::<()>();
        let reader = {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                store
                    .read(move |conn| {
                        let _ = entered.send(());
                        let _ = wait_release.recv();
                        last_global_position(conn)
                    })
                    .await
            })
        };
        tokio::task::spawn_blocking(move || wait_entered.recv().unwrap())
            .await
            .unwrap();

        let exclusive = {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                drop(store.exclude_readers().await);
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !exclusive.is_finished(),
            "exclusion did not wait for the read"
        );

        release.send(()).unwrap();
        reader.await.unwrap().unwrap();
        exclusive.await.unwrap();
    }

    #[test]
    fn debug_output_never_shows_content_bytes() {
        let shown = format!("{:?}", with_content("run-a", 1, &[b"top secret"]));

        assert!(!shown.contains("top secret"));
        assert!(!shown.contains("116, 111, 112"), "bytes as numbers");
        assert!(shown.contains("content_lens: [10]"));
    }
}
