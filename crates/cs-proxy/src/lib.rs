//! The model-call proxy (feature 03): a transparent passthrough in front of the
//! Anthropic Messages API that records metadata about each call.
//!
//! Clients such as Claude Code point `ANTHROPIC_BASE_URL` at the proxy's
//! loopback address. Requests and responses pass through unchanged (see
//! `.kiro/specs/03-passthrough-proxy/design.md`, "Headers" for the few rules);
//! a copy of each response feeds a bounded observer, and the resulting record
//! goes to a bounded queue that never slows or fails the call.
//!
//! | Module | Contract | Task |
//! |---|---|---|
//! | [`headers`] | header rules and run grouping | 1 (`forward`) |
//! | [`trace`] | W3C `traceparent` | 1 (`forward`) |
//! | [`forward`] | the proxy service | 1 (`forward`) |
//! | [`observe`] | usage and metadata from a response copy | 2 (`observe`) |
//! | [`recorder`] | bounded queue into the store | 3 (`recorder`) |

pub mod forward;
pub mod headers;
pub mod observe;
pub mod recorder;
pub mod trace;

pub use forward::{Proxy, ProxyBody, ProxyConfig, Upstream, UpstreamError};
pub use observe::{Observed, ResponseObserver};
pub use recorder::{CallSink, PendingCall, Recorder, RecorderStats};
