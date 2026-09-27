//! Bounded, drop-and-count recording of proxied calls (design, requirement 3).
//! Owned by task 3 (`recorder`).
//!
//! [`CallSink::submit`] never waits: when the queue is full the call is dropped
//! and counted. A task drains the queue and appends one `llm.call` event per
//! call through [`cs_store::Store::append`]; a store error is logged (never
//! with content or credentials) and counted as dropped.
//!
//! Foundation stub: accepts and discards every call.

use std::sync::Arc;

use cs_core::llm::LlmCallRecord;
use cs_store::Store;

/// Default queue capacity.
pub const QUEUE_CAPACITY: usize = 1024;

/// A call ready to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCall {
    pub run_id: String,
    pub record: LlmCallRecord,
    /// Request and response bodies, only while capture is on and within the
    /// store's caps; empty otherwise.
    pub content: Vec<Vec<u8>>,
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
#[derive(Debug)]
pub struct Recorder {
    _store: Arc<Store>,
}

impl Recorder {
    /// Starts the drain task on the current tokio runtime.
    pub fn start(store: Arc<Store>, _capacity: usize) -> Self {
        Self { _store: store }
    }

    pub fn stats(&self) -> RecorderStats {
        RecorderStats::default()
    }

    /// Stops accepting calls and waits until every queued call is written.
    pub async fn shutdown(&self) {}
}

impl CallSink for Recorder {
    fn submit(&self, _call: PendingCall) {}
}
