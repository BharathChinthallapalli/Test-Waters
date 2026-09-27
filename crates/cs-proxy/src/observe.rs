//! Metadata from a copy of the upstream response (design, requirement 4).
//! Owned by task 2 (`observe`).
//!
//! [`ResponseObserver`] is fed every response chunk, in order, after the chunk
//! was sent to the client. It must never fail and must bound its memory: an SSE
//! line or a JSON body over its cap is skipped and the fields it would have
//! given stay `None`.
//!
//! Foundation stub: counts bytes and reads the content type only.

use std::collections::BTreeMap;

use cs_core::llm::Usage;
use http::{HeaderMap, StatusCode};

/// What the observer learned about one response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// The response was `text/event-stream`.
    pub streamed: bool,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub stop_reason: Option<String>,
    /// `error.type` from an error body or an SSE `error` event.
    pub error_type: Option<String>,
    /// Provider-reported; `message_delta` usage overwrites `message_start`'s.
    pub usage: Option<Usage>,
    /// `request-id`, `retry-after`, `x-should-retry`, `anthropic-ratelimit-*`.
    pub rate_limit_headers: BTreeMap<String, String>,
    pub response_bytes: u64,
    /// A stream carried `message_stop`.
    pub saw_message_stop: bool,
}

/// Incrementally reads one response.
#[derive(Debug)]
pub struct ResponseObserver {
    observed: Observed,
}

impl ResponseObserver {
    /// Starts observing a response with this status and these headers.
    pub fn new(_status: StatusCode, headers: &HeaderMap) -> Self {
        let streamed = headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        Self {
            observed: Observed {
                streamed,
                ..Observed::default()
            },
        }
    }

    /// Feeds the next chunk of the body.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.observed.response_bytes += chunk.len() as u64;
    }

    /// Ends the body and returns what was learned.
    pub fn finish(self) -> Observed {
        self.observed
    }
}
