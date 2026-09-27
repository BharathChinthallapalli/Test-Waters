//! Bounded, drop-and-count recording of proxied calls (design, requirement 3).
//! Owned by task 3 (`recorder`).
//!
//! [`CallSink::submit`] never waits: it hands the call to a bounded
//! `tokio::sync::mpsc` queue with `try_send`, and when the queue is full (or
//! already shut down) the call is dropped and counted. One task drains the
//! queue and appends one `llm.call` event per call through
//! [`cs_store::Store::append`], one at a time: the store's writer thread
//! serializes appends anyway. A failed append is counted as dropped and never
//! fails the call, which has long been answered.
//!
//! # Content without its key
//! With capture on, the store loads the content key on the first append that
//! carries content and refuses the whole append if it can't (a locked keychain,
//! say). The recorder then appends the call again without its content and with
//! `contentTruncated: true`, so the metadata is kept and only the bodies are
//! lost. That call counts as recorded.
//!
//! # Logs
//! Logs name counts and error kinds only, never a record, its content or a
//! header, and are rate-limited to one warning per [`WARN_INTERVAL`] each for
//! drops, store failures and content left out, so a full queue or a broken
//! store can't flood the log.
//!
//! # Shutdown
//! The queue's only sender sits in a `Mutex<Option<_>>`; [`Recorder::shutdown`]
//! takes it out and drops it, so later submits find no sender and count as
//! dropped. With the sender gone the channel closes: the drain task still
//! receives every call already queued (`recv` returns `None` only once the
//! channel is closed *and* empty), appends them, and ends. `shutdown` then
//! awaits the task's `JoinHandle`, held behind an async mutex across the wait,
//! so every call (concurrent or repeated) returns only once the queue is
//! drained. A call cancelled mid-wait leaves the handle for the next one.
//!
//! # A drain task that ends early
//! If the task stops before the queue is closed and empty (its runtime is
//! dropped, or it panics), the calls it leaves are still counted: the receiver
//! lives in a [`Drain`] whose `Drop` closes the queue and counts every call left
//! in it, and the one being appended at that moment, as dropped. That one may
//! still be committed by the store's writer thread, so the count can be one too
//! high, never too low.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use cs_core::llm::{LLM_CALL_KIND, LlmCallRecord};
use cs_store::{AppendEvent, Store, StoreError};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::task::JoinHandle;

/// Default queue capacity.
pub const QUEUE_CAPACITY: usize = 1024;

/// At most one warning of each kind per interval.
pub const WARN_INTERVAL: Duration = Duration::from_secs(10);

/// A call ready to record.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingCall {
    pub run_id: String,
    pub record: LlmCallRecord,
    /// Request and response bodies, only while capture is on and within the
    /// store's caps; empty otherwise.
    pub content: Vec<Vec<u8>>,
}

/// Shows the content items, the user agent and each rate-limit header value
/// by their length only, never their bytes.
impl fmt::Debug for PendingCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let content_lens: Vec<usize> = self.content.iter().map(Vec::len).collect();
        f.debug_struct("PendingCall")
            .field("run_id", &self.run_id)
            .field("record", &RedactedRecord(&self.record))
            .field("content_lens", &content_lens)
            .finish()
    }
}

/// An [`LlmCallRecord`] with the user agent and the header values shown as
/// their lengths.
struct RedactedRecord<'a>(&'a LlmCallRecord);

impl fmt::Debug for RedactedRecord<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // No `..`: a new field has to be placed here, redacted or not.
        let LlmCallRecord {
            provider,
            method,
            path,
            status,
            outcome,
            streamed,
            model,
            request_id,
            stop_reason,
            error_type,
            usage,
            started_at_ms,
            ttfb_ms,
            duration_ms,
            request_bytes,
            response_bytes,
            rate_limit_headers,
            trace_id,
            user_agent,
            content_truncated,
        } = self.0;
        let header_lens: BTreeMap<&str, Len> = rate_limit_headers
            .iter()
            .map(|(name, value)| (name.as_str(), Len(value.len())))
            .collect();
        f.debug_struct("LlmCallRecord")
            .field("provider", provider)
            .field("method", method)
            .field("path", path)
            .field("status", status)
            .field("outcome", outcome)
            .field("streamed", streamed)
            .field("model", model)
            .field("request_id", request_id)
            .field("stop_reason", stop_reason)
            .field("error_type", error_type)
            .field("usage", usage)
            .field("started_at_ms", started_at_ms)
            .field("ttfb_ms", ttfb_ms)
            .field("duration_ms", duration_ms)
            .field("request_bytes", request_bytes)
            .field("response_bytes", response_bytes)
            .field("rate_limit_headers", &header_lens)
            .field("trace_id", trace_id)
            .field(
                "user_agent",
                &user_agent.as_ref().map(|agent| Len(agent.len())),
            )
            .field("content_truncated", content_truncated)
            .finish()
    }
}

/// A redacted value, shown by its length only.
struct Len(usize);

impl fmt::Debug for Len {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0)
    }
}

/// Where the proxy hands finished calls. Implementations must not block.
pub trait CallSink: Send + Sync + 'static {
    fn submit(&self, call: PendingCall);
}

/// Counters for `health.proxy`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecorderStats {
    pub calls_recorded: u64,
    pub records_dropped: u64,
}

/// The production sink: a bounded queue drained into the store.
pub struct Recorder {
    /// `None` once shut down. The only sender: dropping it closes the queue.
    queue: Mutex<Option<mpsc::Sender<PendingCall>>>,
    /// The drain task; `None` once it has been awaited to the end.
    drain: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    shared: Arc<Shared>,
}

/// State shared between the submitters and the drain task.
struct Shared {
    recorded: AtomicU64,
    dropped: AtomicU64,
    drop_warning: RateLimit,
    store_warning: RateLimit,
    content_warning: RateLimit,
}

impl Recorder {
    /// Starts the drain task on the current tokio runtime. A `capacity` of 0 is
    /// taken as 1 (a channel can't be empty-sized).
    ///
    /// # Panics
    /// Outside a tokio runtime (`tokio::spawn`).
    pub fn start(store: Arc<Store>, capacity: usize) -> Self {
        let capacity = capacity.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        let (sender, receiver) = mpsc::channel(capacity);
        let shared = Arc::new(Shared {
            recorded: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            drop_warning: RateLimit::new(WARN_INTERVAL),
            store_warning: RateLimit::new(WARN_INTERVAL),
            content_warning: RateLimit::new(WARN_INTERVAL),
        });
        // Built before the spawn: a task dropped before its first poll still
        // drops its `Drain`, which counts what was queued.
        let queue = Drain {
            receiver,
            shared: Arc::clone(&shared),
            appending: false,
        };
        let drain = tokio::spawn(drain(store, queue));
        Self {
            queue: Mutex::new(Some(sender)),
            drain: tokio::sync::Mutex::new(Some(drain)),
            shared,
        }
    }

    /// The counters so far; readable while calls are being recorded.
    pub fn stats(&self) -> RecorderStats {
        RecorderStats {
            calls_recorded: self.shared.recorded.load(Ordering::Relaxed),
            records_dropped: self.shared.dropped.load(Ordering::Relaxed),
        }
    }

    /// Stops accepting calls and waits until every queued call is written (or
    /// counted as dropped). Safe to call more than once and concurrently; every
    /// call waits for the drain. See the module docs.
    ///
    /// Waits as long as the store takes; bound it with a timeout if needed.
    pub async fn shutdown(&self) {
        drop(
            self.queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let mut drain = self.drain.lock().await;
        if let Some(task) = drain.as_mut() {
            let joined = task.await;
            // No await in between: the finished handle is never polled again.
            *drain = None;
            if let Err(error) = joined {
                // The task's `Drain` is dropped before its handle reports the
                // error, and has counted every call the task left.
                tracing::error!(
                    cause = if error.is_panic() {
                        "panic"
                    } else {
                        "cancelled"
                    },
                    records_dropped = self.shared.dropped.load(Ordering::Relaxed),
                    "the call recorder's drain task ended early; the calls it left were dropped"
                );
            }
        }
    }
}

impl CallSink for Recorder {
    fn submit(&self, call: PendingCall) {
        let queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let refused = match queue.as_ref() {
            Some(sender) => match sender.try_send(call) {
                Ok(()) => return,
                Err(TrySendError::Full(_)) => "full",
                Err(TrySendError::Closed(_)) => "closed",
            },
            None => "shut down",
        };
        drop(queue);
        self.shared.dropped_on_submit(refused);
    }
}

impl fmt::Debug for Recorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recorder")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl Shared {
    fn dropped_on_submit(&self, queue: &'static str) {
        let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if self.drop_warning.allow(Instant::now()) {
            tracing::warn!(
                queue,
                records_dropped = dropped,
                "call recorder queue refused a call; it was dropped, not recorded"
            );
        }
    }

    fn append_failed(&self, kind: &'static str) {
        let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if self.store_warning.allow(Instant::now()) {
            tracing::warn!(
                error_kind = kind,
                records_dropped = dropped,
                "recording a call in the store failed; it was dropped"
            );
        }
    }

    fn content_left_out(&self, kind: &'static str) {
        if self.content_warning.allow(Instant::now()) {
            tracing::warn!(
                error_kind = kind,
                "a call's content could not be stored; it is recorded without it"
            );
        }
    }

    /// Counts the calls a drain task left when it ended early.
    fn abandoned(&self, calls: u64) {
        let dropped = self.dropped.fetch_add(calls, Ordering::Relaxed) + calls;
        tracing::warn!(
            abandoned = calls,
            records_dropped = dropped,
            "the call recorder stopped before its queue was written; those calls were dropped"
        );
    }
}

/// The drain task's end of the queue. Dropped before the queue is closed and
/// empty, it counts every call left as dropped (see the module docs).
struct Drain {
    receiver: mpsc::Receiver<PendingCall>,
    shared: Arc<Shared>,
    /// A call is off the queue and not yet counted as recorded or dropped.
    appending: bool,
}

impl Drop for Drain {
    fn drop(&mut self) {
        // Closed first, so no submit can slip a call in behind the count.
        self.receiver.close();
        let mut left = u64::from(self.appending);
        while self.receiver.try_recv().is_ok() {
            left += 1;
        }
        if left > 0 {
            self.shared.abandoned(left);
        }
    }
}

/// Appends every queued call until the queue is closed and empty.
async fn drain(store: Arc<Store>, mut queue: Drain) {
    while let Some(call) = queue.receiver.recv().await {
        queue.appending = true;
        let recorded = record(&store, call, &queue.shared).await;
        queue.appending = false;
        match recorded {
            Ok(()) => {
                queue.shared.recorded.fetch_add(1, Ordering::Relaxed);
            }
            Err(kind) => queue.shared.append_failed(kind),
        }
    }
}

/// Appends one call. If its content can't be stored for want of the content
/// key, appends it again without the content, marked `contentTruncated`.
async fn record(store: &Store, call: PendingCall, shared: &Shared) -> Result<(), &'static str> {
    let PendingCall {
        run_id,
        mut record,
        content,
    } = call;
    let had_content = !content.is_empty();
    match store
        .append(to_event(run_id.clone(), &record, content)?)
        .await
    {
        Ok(_) => Ok(()),
        Err(error @ StoreError::Keychain(_)) if had_content => {
            shared.content_left_out(error_kind(&error));
            record.content_truncated = Some(true);
            store
                .append(to_event(run_id, &record, Vec::new())?)
                .await
                .map(drop)
                .map_err(|error| error_kind(&error))
        }
        Err(error) => Err(error_kind(&error)),
    }
}

/// The `llm.call` event for a call.
fn to_event(
    run_id: String,
    record: &LlmCallRecord,
    content: Vec<Vec<u8>>,
) -> Result<AppendEvent, &'static str> {
    Ok(AppendEvent {
        body: serde_json::to_value(record).map_err(|_| "serialize")?,
        run_id,
        kind: LLM_CALL_KIND.to_owned(),
        ts_ms: record.started_at_ms,
        content,
    })
}

/// A fixed name per error, safe to log: no message, path or value.
fn error_kind(error: &StoreError) -> &'static str {
    match error {
        StoreError::InvalidEvent(_) => "invalidEvent",
        StoreError::Keychain(_) => "keychain",
        StoreError::Chain(_) => "chain",
        StoreError::Hash(_) => "hash",
        StoreError::Sqlite(_) => "sqlite",
        StoreError::Closed => "closed",
        StoreError::WriterFailed => "writerFailed",
        StoreError::TaskFailed => "taskFailed",
    }
}

/// Lets one caller through per interval, lock-free.
struct RateLimit {
    origin: Instant,
    interval_ms: u64,
    /// Milliseconds after `origin` before which nobody else is let through.
    next_ms: AtomicU64,
}

impl RateLimit {
    fn new(interval: Duration) -> Self {
        Self {
            origin: Instant::now(),
            interval_ms: millis(interval),
            next_ms: AtomicU64::new(0),
        }
    }

    /// True for the first caller at or after the end of the last interval.
    fn allow(&self, now: Instant) -> bool {
        let now_ms = millis(now.saturating_duration_since(self.origin));
        let next = self.next_ms.load(Ordering::Relaxed);
        now_ms >= next
            && self
                .next_ms
                .compare_exchange(
                    next,
                    now_ms.saturating_add(self.interval_ms),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::path::Path;
    use std::sync::mpsc as std_mpsc;

    use cs_core::llm::{CallOutcome, Usage};
    use cs_store::ContentKey;
    use cs_store::secrets::{
        IfMissing, InMemorySecretStore, KEY_LEN, KeychainUnavailable, SecretStore,
    };
    use serde_json::{Value, json};
    use tracing::field::{Field, Visit};
    use tracing::span;
    use tracing::subscriber::DefaultGuard;
    use tracing::{Dispatch, Event, Metadata, Subscriber};

    use super::*;

    const KEY: [u8; KEY_LEN] = [7; KEY_LEN];
    const TS: u64 = 1_790_000_000_000;
    const TS_I64: i64 = TS as i64;
    /// Markers that must never reach a log line.
    const UA_MARKER: &str = "ua-marker-4c1f";
    const HEADER_MARKER: &str = "header-marker-8e2a";
    const CONTENT_MARKER: &[u8] = b"content-marker-19bd";

    fn open(dir: &Path) -> Arc<Store> {
        Arc::new(Store::open(dir, Arc::new(InMemorySecretStore::new(KEY))).unwrap())
    }

    fn record(n: u64) -> LlmCallRecord {
        LlmCallRecord {
            provider: "anthropic".into(),
            method: "POST".into(),
            path: "/v1/messages".into(),
            status: 200,
            outcome: CallOutcome::Completed,
            streamed: true,
            model: Some("claude-opus-5".into()),
            request_id: Some(format!("req_{n}")),
            stop_reason: Some("end_turn".into()),
            error_type: None,
            usage: Some(Usage {
                input_tokens: 10 + n,
                output_tokens: 20,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(5),
            }),
            started_at_ms: TS + n,
            ttfb_ms: Some(120),
            duration_ms: 900,
            request_bytes: 512,
            response_bytes: 2048,
            rate_limit_headers: BTreeMap::from([(
                "anthropic-ratelimit-requests-remaining".to_owned(),
                HEADER_MARKER.to_owned(),
            )]),
            trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
            user_agent: Some(UA_MARKER.into()),
            content_truncated: None,
        }
    }

    fn call(run: &str, n: u64) -> PendingCall {
        PendingCall {
            run_id: run.to_owned(),
            record: record(n),
            content: Vec::new(),
        }
    }

    fn address(bytes: &[u8]) -> String {
        cs_store::content_address(&ContentKey::take(&mut { KEY }), bytes)
    }

    #[derive(Debug, PartialEq)]
    struct Row {
        run_id: String,
        kind: String,
        ts_ms: i64,
        body: Value,
    }

    async fn rows(store: &Store) -> Vec<Row> {
        store
            .read(|conn| {
                let mut statement = conn
                    .prepare("SELECT run_id, kind, ts_ms, body FROM events ORDER BY global_pos")?;
                statement
                    .query_map([], |row| {
                        let body: String = row.get(3)?;
                        Ok(Row {
                            run_id: row.get(0)?,
                            kind: row.get(1)?,
                            ts_ms: row.get(2)?,
                            body: serde_json::from_str(&body).unwrap(),
                        })
                    })?
                    .collect()
            })
            .await
            .unwrap()
    }

    async fn assert_verifies(store: &Store, events: u64) {
        let result = cs_store::verify::verify(store).await.unwrap();
        assert!(result.ok, "{result:?}");
        assert_eq!(result.events_checked, events);
    }

    /// Polls `stats` until `done` holds, for at most 10 s.
    async fn wait_for(recorder: &Recorder, done: impl Fn(RecorderStats) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(recorder.stats()) {
            assert!(Instant::now() < deadline, "stats: {:?}", recorder.stats());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn calls_land_as_llm_call_events_with_their_run_body_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), QUEUE_CAPACITY);
        let calls = [
            call("cc-session-a", 1),
            call("run-b", 2),
            call("cc-session-a", 3),
        ];

        for call in &calls {
            recorder.submit(call.clone());
        }
        // Recorded while running, not only on shutdown.
        wait_for(&recorder, |stats| stats.calls_recorded == 3).await;

        let expected: Vec<Row> = calls
            .iter()
            .map(|call| Row {
                run_id: call.run_id.clone(),
                kind: LLM_CALL_KIND.to_owned(),
                ts_ms: i64::try_from(call.record.started_at_ms).unwrap(),
                body: serde_json::to_value(&call.record).unwrap(),
            })
            .collect();
        assert_eq!(rows(&store).await, expected);
        let body: LlmCallRecord = serde_json::from_value(expected[1].body.clone()).unwrap();
        assert_eq!(body, calls[1].record, "the body reads back as the record");
        assert_verifies(&store, 3).await;
        recorder.shutdown().await;
        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: 3,
                records_dropped: 0
            }
        );
    }

    /// A content key that waits for the test's go-ahead: an append that needs
    /// it blocks the drain task, like a keychain unlock prompt would.
    struct GatedSecrets {
        entered: Mutex<std_mpsc::Sender<()>>,
        release: Mutex<std_mpsc::Receiver<()>>,
    }

    impl SecretStore for GatedSecrets {
        fn content_key(&self, _: IfMissing) -> Result<[u8; KEY_LEN], KeychainUnavailable> {
            let _ = self.entered.lock().unwrap().send(());
            let _ = self.release.lock().unwrap().recv();
            Ok(KEY)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_queue_drops_and_counts_without_waiting_for_a_blocked_store() {
        let dir = tempfile::tempdir().unwrap();
        // Capture left on by an earlier run: the next open loads the key
        // lazily, on the first append that carries content.
        let first = open(dir.path());
        assert!(first.set_capture_content(true).await.unwrap());
        first.close().await.unwrap();
        drop(first);
        let (entered_tx, entered) = std_mpsc::channel();
        let (release, release_rx) = std_mpsc::channel();
        let secrets = GatedSecrets {
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
        };
        let store = Arc::new(Store::open(dir.path(), Arc::new(secrets)).unwrap());
        let recorder = Recorder::start(Arc::clone(&store), 1);

        recorder.submit(PendingCall {
            content: vec![b"request".to_vec()],
            ..call("run", 0)
        });
        tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(10)))
            .await
            .unwrap()
            .expect("the drain task is blocked on the store");

        let started = Instant::now();
        for n in 1..=10_000 {
            recorder.submit(call("run", n));
        }
        let elapsed = started.elapsed();
        println!("10000 submits against a blocked store took {elapsed:?}");
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        // One fits in the queue; every other one is dropped and counted.
        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: 0,
                records_dropped: 9_999
            }
        );

        release.send(()).unwrap();
        recorder.shutdown().await;
        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: 2,
                records_dropped: 9_999
            }
        );
        let ts: Vec<i64> = rows(&store).await.iter().map(|row| row.ts_ms).collect();
        assert_eq!(ts, [TS_I64, TS_I64 + 1]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_append_is_dropped_and_later_calls_still_land() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), QUEUE_CAPACITY);

        recorder.submit(call(&"r".repeat(300), 1));
        recorder.submit(call("run", 2));
        recorder.shutdown().await;

        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: 1,
                records_dropped: 1
            }
        );
        let rows = rows(&store).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].run_id.as_str(), rows[0].ts_ms),
            ("run", TS_I64 + 2)
        );
        assert_verifies(&store, 1).await;
    }

    // The default (current-thread) runtime: the drain task doesn't run until
    // the test awaits, so every submit before that is still queued.
    #[tokio::test]
    async fn shutdown_drains_the_queue_and_refuses_later_calls() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), 64);

        for n in 0..50 {
            recorder.submit(call("run", n));
        }
        assert_eq!(recorder.stats(), RecorderStats::default());
        recorder.shutdown().await;
        assert_eq!(recorder.stats().calls_recorded, 50);
        assert_eq!(rows(&store).await.len(), 50);

        recorder.submit(call("run", 50));
        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: 50,
                records_dropped: 1
            }
        );
        // Twice, and concurrently: each returns.
        tokio::join!(recorder.shutdown(), recorder.shutdown());
        recorder.shutdown().await;
        assert_eq!(rows(&store).await.len(), 50);
    }

    #[tokio::test]
    async fn concurrent_shutdowns_both_wait_for_the_drain() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), 64);
        for n in 0..20 {
            recorder.submit(call("run", n));
        }

        let first = async {
            recorder.shutdown().await;
            recorder.stats().calls_recorded
        };
        let second = async {
            recorder.shutdown().await;
            recorder.stats().calls_recorded
        };
        assert_eq!(tokio::join!(first, second), (20, 20));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn content_is_stored_by_address_only_while_capture_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), QUEUE_CAPACITY);
        let request = b"{\"messages\":[]}".to_vec();
        let response = CONTENT_MARKER.to_vec();

        assert!(store.set_capture_content(true).await.unwrap());
        recorder.submit(PendingCall {
            content: vec![request.clone(), response.clone()],
            ..call("run", 1)
        });
        wait_for(&recorder, |stats| stats.calls_recorded == 1).await;
        assert!(!store.set_capture_content(false).await.unwrap());
        recorder.submit(PendingCall {
            content: vec![b"ignored while capture is off".to_vec()],
            ..call("run", 2)
        });
        recorder.shutdown().await;

        let rows = rows(&store).await;
        assert_eq!(
            rows[0].body["content"],
            json!([address(&request), address(&response)])
        );
        assert!(rows[1].body.get("content").is_none(), "{:?}", rows[1].body);
        assert_eq!(rows[1].body, serde_json::to_value(record(2)).unwrap());
        assert_eq!(store.blob_count().await.unwrap(), 2);
        assert_verifies(&store, 2).await;
    }

    #[test]
    fn debug_shows_sizes_not_content_user_agent_or_header_values() {
        let call = PendingCall {
            content: vec![CONTENT_MARKER.to_vec()],
            ..call("run", 1)
        };
        for shown in [format!("{call:?}"), format!("{call:#?}")] {
            for marker in [UA_MARKER, HEADER_MARKER, "content-marker"] {
                assert!(!shown.contains(marker), "{marker} in {shown}");
            }
            assert!(!shown.contains(&format!("{:?}", CONTENT_MARKER.to_vec())));
        }
        let shown = format!("{call:?}");
        assert!(shown.contains("content_lens: [19]"), "{shown}");
        assert!(shown.contains("user_agent: Some(<14 bytes>)"), "{shown}");
        assert!(
            shown.contains(
                "rate_limit_headers: {\"anthropic-ratelimit-requests-remaining\": <18 bytes>}"
            ),
            "header names stay: {shown}"
        );
        // The rest of the record is shown as it is.
        for kept in [
            "run_id: \"run\"",
            "model: Some(\"claude-opus-5\")",
            "request_id: Some(\"req_1\")",
            "trace_id: \"0af7651916cd43dd8448eb211c80319c\"",
            "started_at_ms: 1790000000001",
            "content_truncated: None",
        ] {
            assert!(shown.contains(kept), "{kept} in {shown}");
        }
    }

    #[test]
    fn calls_queued_when_the_runtime_drops_are_counted_as_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // The runtime is never driven, so its drain task never runs.
        let recorder = {
            let _entered = runtime.enter();
            Recorder::start(Arc::clone(&store), 64)
        };
        for n in 0..10 {
            recorder.submit(call("run", n));
        }
        assert_eq!(recorder.stats(), RecorderStats::default());

        drop(runtime);
        let lost = RecorderStats {
            calls_recorded: 0,
            records_dropped: 10,
        };
        assert_eq!(recorder.stats(), lost);

        // A later shutdown finds the task cancelled and returns; the queue is
        // closed, so a later submit is dropped too.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(recorder.shutdown());
        assert_eq!(recorder.stats(), lost);
        recorder.submit(call("run", 10));
        assert_eq!(recorder.stats().records_dropped, 11);
        assert!(runtime.block_on(rows(&store)).is_empty());
    }

    // Current-thread runtime, for the log capture (see below).
    #[tokio::test]
    async fn without_the_content_key_the_metadata_is_recorded_without_content() {
        let (logs, _other, _guard) = Logs::capture();
        let dir = tempfile::tempdir().unwrap();
        // Capture left on by an earlier run: the next open loads the key on the
        // first append that carries content, and here it can't.
        let first = open(dir.path());
        assert!(first.set_capture_content(true).await.unwrap());
        first.close().await.unwrap();
        drop(first);
        let secrets = InMemorySecretStore::unavailable("keychain locked");
        let store = Arc::new(Store::open(dir.path(), Arc::new(secrets)).unwrap());
        let recorder = Recorder::start(Arc::clone(&store), QUEUE_CAPACITY);

        for n in 1..=2 {
            recorder.submit(PendingCall {
                content: vec![b"request".to_vec(), CONTENT_MARKER.to_vec()],
                ..call("run", n)
            });
        }
        recorder.submit(call("run", 3));
        recorder.shutdown().await;

        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: 3,
                records_dropped: 0
            }
        );
        let truncated = |n| LlmCallRecord {
            content_truncated: Some(true),
            ..record(n)
        };
        let bodies: Vec<Value> = rows(&store).await.into_iter().map(|row| row.body).collect();
        assert_eq!(
            bodies,
            [
                serde_json::to_value(truncated(1)).unwrap(),
                serde_json::to_value(truncated(2)).unwrap(),
                // No content, so no key needed and nothing to mark.
                serde_json::to_value(record(3)).unwrap(),
            ]
        );
        assert_eq!(bodies[0]["contentTruncated"], json!(true));
        assert_eq!(store.blob_count().await.unwrap(), 0);
        assert!(store.capture_content(), "the user's setting is kept");
        assert_verifies(&store, 3).await;

        let lines = logs.lines();
        assert_eq!(lines.len(), 1, "rate-limited: {lines:?}");
        assert!(lines[0].starts_with("WARN"), "{lines:?}");
        assert!(lines[0].contains("error_kind=\"keychain\""), "{lines:?}");
        assert!(!lines[0].contains("locked"), "no error message: {lines:?}");
        assert_no_markers(&lines);
    }

    #[test]
    fn the_rate_limit_lets_one_caller_through_per_interval() {
        let limit = RateLimit::new(WARN_INTERVAL);
        let t0 = limit.origin;

        assert!(limit.allow(t0));
        assert!(!limit.allow(t0));
        assert!(!limit.allow(t0 + Duration::from_millis(9_999)));
        assert!(limit.allow(t0 + WARN_INTERVAL));
        assert!(!limit.allow(t0 + WARN_INTERVAL + Duration::from_secs(1)));
        // A clock before the origin counts as the origin.
        let fresh = RateLimit::new(WARN_INTERVAL);
        assert!(fresh.allow(t0));

        let limit = Arc::new(RateLimit::new(WARN_INTERVAL));
        let now = Instant::now();
        let allowed: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let limit = Arc::clone(&limit);
                    scope.spawn(move || (0..1_000).filter(|_| limit.allow(now)).count())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(allowed, 1, "one of 8000 concurrent callers");
    }

    /// Collects every `cs_proxy` event as `LEVEL field=value …`.
    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<String>>>);

    impl Logs {
        /// Captures this thread's events until the guards drop.
        ///
        /// With a single dispatcher registered, tracing-core 0.1.36 rebuilds a
        /// callsite's interest from the *calling thread's* default
        /// (`callsite.rs`, `Dispatchers::rebuilder`), so a callsite first hit on
        /// another test's thread would be cached as "never" and this test would
        /// see nothing. A second live dispatcher makes every rebuild use the
        /// registered list instead.
        fn capture() -> (Self, Dispatch, DefaultGuard) {
            let other = Dispatch::new(Self::default());
            let logs = Self::default();
            let guard = tracing::subscriber::set_default(logs.clone());
            (logs, other, guard)
        }

        fn lines(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    struct Line<'a>(&'a mut String);

    impl Visit for Line<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    impl Subscriber for Logs {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }

        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

        fn event(&self, event: &Event<'_>) {
            if !event.metadata().target().starts_with("cs_proxy") {
                return;
            }
            let mut line = event.metadata().level().to_string();
            event.record(&mut Line(&mut line));
            self.0.lock().unwrap().push(line);
        }

        fn enter(&self, _: &span::Id) {}

        fn exit(&self, _: &span::Id) {}
    }

    fn assert_no_markers(lines: &[String]) {
        let all = lines.join("\n");
        for marker in [
            UA_MARKER,
            HEADER_MARKER,
            "content-marker",
            "claude-opus-5",
            "req_",
        ] {
            assert!(!all.contains(marker), "log holds {marker}: {all}");
        }
    }

    // Current-thread runtime: the drain task runs on this thread, where the
    // capturing subscriber is the default.
    #[tokio::test]
    async fn drops_log_one_rate_limited_warning_without_the_record() {
        let (logs, _other, _guard) = Logs::capture();
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), 1);

        // The drain task can't run between these: one is queued, 999 dropped.
        for n in 0..1_000 {
            recorder.submit(PendingCall {
                content: vec![CONTENT_MARKER.to_vec()],
                ..call("run", n)
            });
        }
        recorder.shutdown().await;
        recorder.submit(call("run", 1_000));

        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: 1,
                records_dropped: 1_000
            }
        );
        let lines = logs.lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("WARN"), "{lines:?}");
        assert!(lines[0].contains("queue=\"full\""), "{lines:?}");
        assert!(lines[0].contains("records_dropped=1"), "{lines:?}");
        assert_no_markers(&lines);
    }

    #[tokio::test]
    async fn a_store_failure_logs_its_kind_without_the_record() {
        let (logs, _other, _guard) = Logs::capture();
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), QUEUE_CAPACITY);

        recorder.submit(PendingCall {
            content: vec![CONTENT_MARKER.to_vec()],
            ..call(&"r".repeat(300), 1)
        });
        recorder.submit(call(&"r".repeat(300), 2));
        recorder.shutdown().await;

        assert_eq!(recorder.stats().records_dropped, 2);
        let lines = logs.lines();
        assert_eq!(lines.len(), 1, "rate-limited: {lines:?}");
        assert!(
            lines[0].contains("error_kind=\"invalidEvent\""),
            "{lines:?}"
        );
        assert!(!lines[0].contains("rrrr"), "no run id: {lines:?}");
        assert_no_markers(&lines);
    }

    /// Throughput, one append per store call, next to the store alone
    /// (sequential `Store::append` of the same events) on the same disk. Run in
    /// release:
    /// `cargo test --release -p cs-proxy --lib throughput -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "measurement; run in release"]
    async fn throughput_of_10_000_records() {
        const N: u64 = 10_000;
        #[allow(clippy::cast_precision_loss)]
        fn rate(elapsed: Duration) -> f64 {
            N as f64 / elapsed.as_secs_f64()
        }
        let calls: Vec<PendingCall> = (0..N).map(|n| call(&format!("run-{}", n % 8), n)).collect();

        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let started = Instant::now();
        for call in calls.clone() {
            let event = to_event(call.run_id, &call.record, call.content).unwrap();
            store.append(event).await.unwrap();
        }
        let alone = started.elapsed();

        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let recorder = Recorder::start(Arc::clone(&store), calls.len());
        let started = Instant::now();
        for call in calls {
            recorder.submit(call);
        }
        let submitted = started.elapsed();
        recorder.shutdown().await;
        let recorded = started.elapsed();

        assert_eq!(
            recorder.stats(),
            RecorderStats {
                calls_recorded: N,
                records_dropped: 0
            }
        );
        assert_verifies(&store, N).await;
        println!(
            "{N} records: store alone {alone:?} ({:.0}/s); recorder: submitted in {submitted:?}, \
             recorded in {recorded:?} ({:.0}/s)",
            rate(alone),
            rate(recorded)
        );
    }
}
