//! The proxy service (design, requirements 1, 2, 5 and 8). Owned by task 1
//! (`forward`).
//!
//! [`Proxy::handle`] guards the request (Origin, Host), buffers the request
//! body (at most [`MAX_REQUEST_BYTES`]), sends it upstream with
//! [`crate::headers::to_upstream`], and streams the response back chunk by chunk
//! while feeding a [`crate::ResponseObserver`]. When the body ends, or the
//! client goes away, it builds a [`crate::PendingCall`] and submits it to the
//! [`crate::CallSink`]. Requests whose path doesn't start with `/v1/` are
//! forwarded but not recorded.
//!
//! Foundation stub: answers every request with 501 in the Anthropic error shape.

use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;

use crate::recorder::CallSink;

/// Largest request body the proxy accepts (the Messages API's own limit is
/// 32 MB); larger gets 413 `request_too_large`.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// The body type of every proxy response.
pub type ProxyBody = BoxBody<Bytes, Infallible>;

/// Where calls are forwarded: an `https` base URL, or `http` to a loopback
/// address (tests only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    base: String,
}

/// Why an upstream URL was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamError(pub String);

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UpstreamError {}

impl Upstream {
    /// The Anthropic API.
    pub const ANTHROPIC: &str = "https://api.anthropic.com";

    /// Parses and checks an upstream base URL.
    pub fn parse(base: &str) -> Result<Self, UpstreamError> {
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    /// The base URL, without a trailing `/`.
    pub fn as_str(&self) -> &str {
        &self.base
    }
}

/// Settings for one proxy listener.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub upstream: Upstream,
    /// Run for calls without a run header, such as `proxy-<startedAtMs>`.
    pub default_run_id: String,
    /// The bound port, for the `Host` check.
    pub listen_port: u16,
}

/// Reports whether content capture is on right now.
pub type CaptureSwitch = Arc<dyn Fn() -> bool + Send + Sync>;

/// The proxy service. Cheap to clone; share one per listener.
#[derive(Clone)]
pub struct Proxy {
    _config: Arc<ProxyConfig>,
    _sink: Arc<dyn CallSink>,
    _capture: CaptureSwitch,
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proxy").finish_non_exhaustive()
    }
}

impl Proxy {
    /// Builds the proxy and its HTTP client.
    pub fn new(config: ProxyConfig, sink: Arc<dyn CallSink>, capture: CaptureSwitch) -> Self {
        Self {
            _config: Arc::new(config),
            _sink: sink,
            _capture: capture,
        }
    }

    /// Handles one request. Never fails: problems become Anthropic-shaped
    /// error responses.
    pub async fn handle(&self, _request: Request<Incoming>) -> Response<ProxyBody> {
        error_response(
            StatusCode::NOT_IMPLEMENTED,
            "api_error",
            "The Callsheet proxy is not implemented yet.",
        )
    }
}

/// An error in the Anthropic shape:
/// `{"type":"error","error":{"type":…,"message":…}}`.
pub fn error_response(status: StatusCode, error_type: &str, message: &str) -> Response<ProxyBody> {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": error_type, "message": message },
    });
    let mut response = Response::new(Full::new(Bytes::from(body.to_string())).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}
