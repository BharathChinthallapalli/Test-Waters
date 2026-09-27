//! W3C Trace Context for recorded calls. Owned by task 1 (`forward`).
//!
//! The trace id comes from a valid `traceparent` request header, or is
//! generated when there is none. A generated id is recorded, never forwarded.
//!
//! Foundation stub: always reports a placeholder id.

use http::HeaderMap;

/// The trace a call belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContext {
    /// 32 lower-case hex characters, never all zeros once implemented.
    pub trace_id: String,
    /// The id came from the client's `traceparent`.
    pub from_client: bool,
}

/// Reads `traceparent`, or starts a new trace.
pub fn trace_context(_request: &HeaderMap) -> TraceContext {
    TraceContext {
        trace_id: "0".repeat(32),
        from_client: false,
    }
}
