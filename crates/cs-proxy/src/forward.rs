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
//! The request is forwarded to the upstream base URL plus the request's path
//! and query. The `url` crate reqwest takes percent-encodes a few characters
//! hyper accepts (`'` in a query; `"`, `{`, `}` and non-ASCII in a path), so
//! those go out encoded; a target it would change in meaning (`..` or `.`
//! segments) gets 400 rather than going out changed. Claude Code's own
//! targets (`/v1/messages?beta=true`, `/v1/models?limit=1000`, `/api/hello`)
//! pass byte for byte.
//!
//! The response body is a `RelayBody`: each upstream frame goes to hyper as it
//! arrives, and the observer (and the capture copy) sees a chunk only on the
//! next poll, after hyper took it. Its `Drop` submits the record exactly once:
//! a body that ended normally is `completed` (or `upstreamError`), one hyper
//! dropped early because the client went away is `clientCancelled`, and
//! dropping it drops the upstream response, which closes that connection.
//! hyper also drops a body without polling it to the end once a
//! `content-length` response is fully written, so the body counts relayed
//! bytes against the length the client was told (so a `content-length` the
//! upstream sent next to `transfer-encoding` is ignored, as
//! [`crate::headers::to_client`] drops it). hyper likewise stops after a
//! trailers frame, the last frame there is, so one ends the body normally. A
//! client that leaves before the upstream's response head arrives is recorded
//! as `clientCancelled` with status 0 (`AwaitingResponse`).
//!
//! The observer is another task's code running on every chunk, so it runs
//! behind `catch_unwind` (`GuardedObserver`): a panic turns it off for that
//! call, is logged once without the body, and the call is still recorded with
//! the bytes the proxy counted. The client's stream never sees it.
//!
//! The upstream client is reqwest 0.13.5 with rustls on ring. reqwest's
//! `rustls-no-provider` feature needs a process-wide rustls provider installed
//! before the client is built (reqwest 0.13.5 `src/tls.rs`, "rustls-no-provider",
//! and `default_rustls_crypto_provider` in `src/async_impl/client.rs`, which
//! panics otherwise); [`Proxy::new`] and [`Proxy::try_new`] install ring's
//! with `rustls::crypto::ring::default_provider().install_default()`, as those
//! docs show. Connect timeout 10 s,
//! no total or read timeout (`ClientBuilder::timeout` defaults to none), and
//! `redirect::Policy::none()` so redirects reach the client. reqwest adds
//! `accept: */*` when the client sent no `accept` (`ClientBuilder::new`); that is
//! the one header the upstream can see that the client didn't send, besides
//! `host`.
//!
//! An upstream failure part-way through a body ends the client's body with an
//! [`UpstreamBodyFailed`] error, never a normal end: hyper 1.11.1 then closes
//! the client connection without ending the body (`poll_write` returns the
//! error before `end_body`, `proto/h1/dispatch.rs`), so a chunked response gets
//! no last chunk and a `content-length` one stays short (RFC 9112, section 8:
//! the client must treat it as incomplete). hyper aborts without flushing what
//! it buffered in the same write pass, which can include the head and the last
//! chunk (when a chunk and the failure are ready together, as HTTP/2's DATA and
//! RST_STREAM can be), so the body first yields `Pending` once, with a wake-up,
//! to let hyper flush, then fails. That flush is best-effort: bytes a full
//! client socket didn't take are lost with the connection. The record says
//! `incomplete`.

use std::fmt;
use std::net::IpAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cs_core::llm::{CallOutcome, LlmCallRecord};
use cs_store::writer::{MAX_CONTENT_BYTES, MAX_CONTENT_ITEMS};
use http::header::{CONTENT_LENGTH, HOST, ORIGIN, USER_AGENT};
use http::{HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Body, Frame, Incoming, SizeHint};

use crate::headers;
use crate::observe::{Observed, ResponseObserver};
use crate::recorder::{CallSink, PendingCall};
use crate::trace;

/// Largest request body the proxy accepts (the Messages API's own limit is
/// 32 MB); larger gets 413 `request_too_large`.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// How long the upstream connection may take to open (TCP and TLS).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest `userAgent` recorded, in bytes.
pub const MAX_USER_AGENT_BYTES: usize = 200;

/// Provider name recorded for every call in feature 03.
const PROVIDER: &str = "anthropic";

/// Only paths under this prefix are recorded.
const RECORDED_PREFIX: &str = "/v1/";

// Capture stores two items: the request body and the response body.
const _: () = assert!(MAX_CONTENT_ITEMS >= 2);

/// The body of every proxy response.
pub type ProxyBody = BoxBody<Bytes, UpstreamBodyFailed>;

/// A response body stopped part-way because the provider's body ended with an
/// error. The server closes the client connection without ending the body, so
/// the client sees a broken response, never a complete one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamBodyFailed;

impl fmt::Display for UpstreamBodyFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the provider's response ended part-way")
    }
}

impl std::error::Error for UpstreamBodyFailed {}

/// Where calls are forwarded: an `https` base URL, or `http` to a loopback
/// address (tests only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    base: String,
    /// The base URL's path without a trailing `/` (usually empty).
    base_path: String,
    /// `host[:port]`, for error messages.
    authority: String,
    loopback_http: bool,
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

    /// Parses and checks an upstream base URL: `https` with a host, or `http`
    /// whose host is a loopback IP address. No user info, query or fragment.
    /// Errors never repeat the URL, which could hold a credential.
    pub fn parse(base: &str) -> Result<Self, UpstreamError> {
        let url = reqwest::Url::parse(base)
            .map_err(|error| UpstreamError(format!("the upstream URL is invalid: {error}")))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(UpstreamError(
                "the upstream URL must not contain a user name or password".into(),
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(UpstreamError(
                "the upstream URL must not have a query or fragment".into(),
            ));
        }
        let host = url.host_str().unwrap_or_default();
        let loopback_ip = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
        let loopback_http = match url.scheme() {
            "https" if !host.is_empty() => false,
            "http" if loopback_ip => true,
            "http" => {
                return Err(UpstreamError(
                    "an http upstream must be a loopback IP address; use https".into(),
                ));
            }
            _ => return Err(UpstreamError("the upstream URL must be https".into())),
        };
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        Ok(Self {
            base: url.as_str().trim_end_matches('/').to_owned(),
            base_path: url.path().trim_end_matches('/').to_owned(),
            authority,
            loopback_http,
        })
    }

    /// The base URL, without a trailing `/`.
    pub fn as_str(&self) -> &str {
        &self.base
    }

    /// The upstream's `host[:port]`.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// The upstream URL for a request's path and query, or `None` if the URL
    /// parser would change what they mean (resolving `..` or `.` segments).
    ///
    /// The `url` crate percent-encodes a few characters hyper accepts (`'` in a
    /// query; `"`, `{`, `}` and non-ASCII in a path). Those go out encoded, which
    /// means the same; the comparison is on percent-decoded bytes for that.
    fn url_for(&self, path_and_query: &str) -> Option<reqwest::Url> {
        let url = reqwest::Url::parse(&format!("{}{path_and_query}", self.base)).ok()?;
        let mut sent = url.path().to_owned();
        if let Some(query) = url.query() {
            sent.push('?');
            sent.push_str(query);
        }
        let sent = sent.strip_prefix(self.base_path.as_str())?;
        (percent_decoded(sent.as_bytes()) == percent_decoded(path_and_query.as_bytes()))
            .then_some(url)
    }
}

/// `%XX` escapes decoded; anything else, malformed escapes included, as is.
fn percent_decoded(input: &[u8]) -> Vec<u8> {
    let hex = |byte: u8| char::from(byte).to_digit(16);
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let escape = input.get(i + 1..i + 3).filter(|_| input[i] == b'%');
        match escape.and_then(|pair| Some(hex(pair[0])? * 16 + hex(pair[1])?)) {
            Some(value) => {
                out.push(value as u8);
                i += 3;
            }
            None => {
                out.push(input[i]);
                i += 1;
            }
        }
    }
    out
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

/// The HTTP client could not be built.
#[derive(Debug)]
pub struct ClientBuildError(reqwest::Error);

impl fmt::Display for ClientBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the proxy's HTTPS client could not be built: {}", self.0)
    }
}

impl std::error::Error for ClientBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// The proxy service. Cheap to clone; share one per listener.
#[derive(Clone)]
pub struct Proxy {
    config: Arc<ProxyConfig>,
    sink: Arc<dyn CallSink>,
    capture: CaptureSwitch,
    /// `None` when the client could not be built; every call then gets 502.
    client: Option<reqwest::Client>,
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proxy").finish_non_exhaustive()
    }
}

impl Proxy {
    /// Builds the proxy and its HTTP client. If the client can't be built (no
    /// usable system root store, for example) the error is logged and every call
    /// gets a 502; use [`Proxy::try_new`] to fail at startup instead.
    pub fn new(config: ProxyConfig, sink: Arc<dyn CallSink>, capture: CaptureSwitch) -> Self {
        let client = build_client(&config.upstream, CONNECT_TIMEOUT)
            .inspect_err(|error| tracing::error!(%error, "proxy HTTP client unavailable"))
            .ok();
        Self {
            config: Arc::new(config),
            sink,
            capture,
            client,
        }
    }

    /// Builds the proxy, failing if its HTTP client can't be built.
    pub fn try_new(
        config: ProxyConfig,
        sink: Arc<dyn CallSink>,
        capture: CaptureSwitch,
    ) -> Result<Self, ClientBuildError> {
        Self::with_connect_timeout(config, sink, capture, CONNECT_TIMEOUT)
    }

    fn with_connect_timeout(
        config: ProxyConfig,
        sink: Arc<dyn CallSink>,
        capture: CaptureSwitch,
        connect_timeout: Duration,
    ) -> Result<Self, ClientBuildError> {
        let client = build_client(&config.upstream, connect_timeout)?;
        Ok(Self {
            config: Arc::new(config),
            sink,
            capture,
            client: Some(client),
        })
    }

    /// Handles one request. Never fails: problems become Anthropic-shaped
    /// error responses.
    pub async fn handle(&self, request: Request<Incoming>) -> Response<ProxyBody> {
        let started = Instant::now();
        let started_at_ms = unix_ms();
        if let Some(why) = self.guard(&request) {
            return error_response(StatusCode::FORBIDDEN, "permission_error", why);
        }
        let (parts, body) = request.into_parts();
        let Some(url) = parts
            .uri
            .path_and_query()
            .map(|path_and_query| path_and_query.as_str())
            .filter(|path_and_query| path_and_query.starts_with('/'))
            .and_then(|path_and_query| self.config.upstream.url_for(path_and_query))
        else {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Callsheet can't forward this request target unchanged.",
            );
        };
        let body = match read_body(&parts.headers, body).await {
            Ok(body) => body,
            Err(refused) => return refused.response(),
        };

        let recorded = parts.uri.path().starts_with(RECORDED_PREFIX);
        let call = AwaitingResponse(recorded.then(|| {
            let capture = (self.capture)().then(|| Capture::new(body.clone()));
            CallStart {
                sink: Arc::clone(&self.sink),
                run_id: headers::run_id(&parts.headers, &self.config.default_run_id),
                method: parts.method.to_string(),
                path: parts.uri.path().to_owned(),
                started,
                started_at_ms,
                request_bytes: body.len() as u64,
                trace_id: trace::trace_context(&parts.headers).trace_id,
                user_agent: user_agent(&parts.headers),
                capture,
            }
        }));

        let Some(client) = &self.client else {
            return self.unreachable(
                call.take(),
                StatusCode::BAD_GATEWAY,
                "api_error",
                "its HTTPS client is unavailable",
            );
        };
        let sent = client
            .request(parts.method.clone(), url)
            .headers(headers::to_upstream(&parts.headers))
            .body(body)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    upstream = %self.config.upstream.authority(),
                    error = %error_chain(&error),
                    "provider unreachable"
                );
                let (status, error_type, why) = unreachable_reply(&error);
                return self.unreachable(call.take(), status, error_type, why);
            }
        };

        let ttfb_ms = elapsed_ms(started);
        let (upstream, upstream_body) =
            http::Response::<reqwest::Body>::from(response).into_parts();
        let client_headers = headers::to_client(&upstream.headers);
        let expected_bytes = if parts.method == Method::HEAD || !may_have_body(upstream.status) {
            Some(0)
        } else {
            content_length(&client_headers)
        };
        let recording = call.take().map(|call| Recording {
            observer: GuardedObserver::new(upstream.status, &upstream.headers),
            status: upstream.status.as_u16(),
            ttfb_ms,
            call,
        });
        let body = RelayBody::new(upstream_body, expected_bytes, recording);
        let mut response = Response::new(BoxBody::new(body));
        *response.status_mut() = upstream.status;
        *response.headers_mut() = client_headers;
        response
    }

    /// Origin and Host checks (requirement 5): why the request is refused, if
    /// it is.
    fn guard(&self, request: &Request<Incoming>) -> Option<&'static str> {
        if request.headers().contains_key(ORIGIN) {
            return Some("Callsheet's proxy refuses requests from web pages.");
        }
        let port = self.config.listen_port;
        let local = |authority: &[u8]| {
            let expected = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
            expected
                .iter()
                .any(|expected| authority.eq_ignore_ascii_case(expected.as_bytes()))
        };
        let mut hosts = request.headers().get_all(HOST).iter();
        let host_ok = match (hosts.next(), hosts.next()) {
            (Some(host), None) => local(host.as_bytes()),
            _ => false,
        };
        let uri_ok = request
            .uri()
            .authority()
            .is_none_or(|authority| local(authority.as_str().as_bytes()));
        (!host_ok || !uri_ok)
            .then_some("Callsheet's proxy only answers requests addressed to its loopback address.")
    }

    /// The proxy's own 502 or 504, recorded as `upstreamUnreachable`. The
    /// message names the upstream host and nothing from the request.
    fn unreachable(
        &self,
        call: Option<CallStart>,
        status: StatusCode,
        error_type: &str,
        why: &str,
    ) -> Response<ProxyBody> {
        let message = format!(
            "Callsheet could not reach the provider at {}: {why}.",
            self.config.upstream.authority()
        );
        let body = error_body(error_type, &message);
        if let Some(mut call) = call {
            let observed = Observed {
                response_bytes: body.len() as u64,
                ..Observed::default()
            };
            if let Some(capture) = &mut call.capture {
                capture.add_response(&body);
            }
            call.submit(
                status.as_u16(),
                CallOutcome::UpstreamUnreachable,
                observed,
                None,
            );
        }
        json_response(status, body)
    }
}

/// Builds the upstream client (see the module docs).
fn build_client(
    upstream: &Upstream,
    connect_timeout: Duration,
) -> Result<reqwest::Client, ClientBuildError> {
    // Err means a provider is already installed (ring is the only one built in).
    let _already_installed = rustls::crypto::ring::default_provider().install_default();
    let mut builder = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .redirect(reqwest::redirect::Policy::none());
    if upstream.loopback_http {
        // A system proxy can't reach this machine's loopback.
        builder = builder.no_proxy();
    }
    builder.build().map_err(ClientBuildError)
}

/// Why a request body wasn't read.
enum BodyRefused {
    TooLarge,
    Unreadable,
}

impl BodyRefused {
    fn response(self) -> Response<ProxyBody> {
        match self {
            Self::TooLarge => error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "The request body is larger than Callsheet's 32 MiB limit.",
            ),
            Self::Unreadable => error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Callsheet could not read the request body.",
            ),
        }
    }
}

/// Buffers the request body: refused over [`MAX_REQUEST_BYTES`] (declared or
/// actual), or if the client stops sending part-way.
async fn read_body(headers: &HeaderMap, body: Incoming) -> Result<Bytes, BodyRefused> {
    if content_length(headers).is_some_and(|length| length > MAX_REQUEST_BYTES as u64) {
        return Err(BodyRefused::TooLarge);
    }
    match Limited::new(body, MAX_REQUEST_BYTES).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            Err(BodyRefused::TooLarge)
        }
        Err(_) => Err(BodyRefused::Unreadable),
    }
}

/// What is known about a recorded call when its request arrives.
struct CallStart {
    sink: Arc<dyn CallSink>,
    run_id: String,
    method: String,
    path: String,
    started: Instant,
    started_at_ms: u64,
    request_bytes: u64,
    trace_id: String,
    user_agent: Option<String>,
    capture: Option<Capture>,
}

impl CallStart {
    fn submit(self, status: u16, outcome: CallOutcome, observed: Observed, ttfb_ms: Option<u64>) {
        let (content, content_truncated) = match self.capture {
            Some(capture) => capture.into_content(),
            None => (Vec::new(), None),
        };
        let duration_ms = elapsed_ms(self.started);
        tracing::debug!(
            method = %self.method,
            path = %self.path,
            status,
            outcome = ?outcome,
            duration_ms,
            "proxied call"
        );
        let record = LlmCallRecord {
            provider: PROVIDER.to_owned(),
            method: self.method,
            path: self.path,
            status,
            outcome,
            streamed: observed.streamed,
            model: observed.model,
            request_id: observed.request_id,
            stop_reason: observed.stop_reason,
            error_type: observed.error_type,
            usage: observed.usage,
            started_at_ms: self.started_at_ms,
            ttfb_ms,
            duration_ms,
            request_bytes: self.request_bytes,
            response_bytes: observed.response_bytes,
            rate_limit_headers: observed.rate_limit_headers,
            trace_id: self.trace_id,
            user_agent: self.user_agent,
            content_truncated,
        };
        self.sink.submit(PendingCall {
            run_id: self.run_id,
            record,
            content,
        });
    }
}

/// A recorded call until the upstream's response head arrives. If the client
/// goes away first, hyper drops [`Proxy::handle`]'s future, and with it the
/// upstream request and this guard, which records `clientCancelled` with
/// status 0 (the client got no response).
struct AwaitingResponse(Option<CallStart>);

impl AwaitingResponse {
    fn take(mut self) -> Option<CallStart> {
        self.0.take()
    }
}

impl Drop for AwaitingResponse {
    fn drop(&mut self) {
        if let Some(call) = self.0.take() {
            call.submit(0, CallOutcome::ClientCancelled, Observed::default(), None);
        }
    }
}

/// Copies of the bodies while capture is on, dropped as soon as they exceed the
/// store's content cap.
struct Capture {
    request: Bytes,
    response: Vec<u8>,
    over_cap: bool,
}

impl Capture {
    fn new(request: Bytes) -> Self {
        let mut capture = Self {
            request,
            response: Vec::new(),
            over_cap: false,
        };
        capture.check_cap(0);
        capture
    }

    fn add_response(&mut self, chunk: &[u8]) {
        if self.check_cap(chunk.len()) {
            self.response.extend_from_slice(chunk);
        }
    }

    /// Whether `more` bytes still fit; drops both copies once they don't.
    fn check_cap(&mut self, more: usize) -> bool {
        let total = self.request.len() + self.response.len() + more;
        if !self.over_cap && total > MAX_CONTENT_BYTES {
            self.over_cap = true;
            self.request = Bytes::new();
            self.response = Vec::new();
        }
        !self.over_cap
    }

    /// `(content, contentTruncated)` for the record.
    fn into_content(self) -> (Vec<Vec<u8>>, Option<bool>) {
        if self.over_cap {
            (Vec::new(), Some(true))
        } else {
            (vec![self.request.to_vec(), self.response], None)
        }
    }
}

/// A recorded call whose response is streaming.
struct Recording {
    call: CallStart,
    observer: GuardedObserver,
    status: u16,
    ttfb_ms: u64,
}

/// Where the observer was when it panicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObserverStage {
    Start,
    Feed,
    Finish,
}

impl ObserverStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Feed => "feed",
            Self::Finish => "finish",
        }
    }
}

/// [`ResponseObserver`] behind `catch_unwind` (see the module docs). After a
/// panic the observer is gone for this call and [`GuardedObserver::finish`]
/// reports only the bytes fed.
struct GuardedObserver {
    observer: Option<ResponseObserver>,
    fed_bytes: u64,
}

impl GuardedObserver {
    fn new(status: StatusCode, headers: &HeaderMap) -> Self {
        Self {
            observer: guarded(ObserverStage::Start, || {
                ResponseObserver::new(status, headers)
            }),
            fed_bytes: 0,
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        self.fed_bytes += chunk.len() as u64;
        let Some(observer) = &mut self.observer else {
            return;
        };
        if guarded(ObserverStage::Feed, || observer.feed(chunk)).is_none() {
            self.observer = None;
        }
    }

    fn finish(self) -> Observed {
        let fed_bytes = self.fed_bytes;
        self.observer
            .and_then(|observer| guarded(ObserverStage::Finish, || observer.finish()))
            .unwrap_or_else(|| Observed {
                response_bytes: fed_bytes,
                ..Observed::default()
            })
    }
}

/// Runs one observer step, `None` if it panicked. The panic is logged without
/// its payload, which could quote the body.
fn guarded<T>(stage: ObserverStage, step: impl FnOnce() -> T) -> Option<T> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        #[cfg(test)]
        observer_panic_hook::trigger(stage);
        step()
    }));
    match result {
        Ok(value) => Some(value),
        Err(payload) => {
            // Dropping a payload can itself panic; this path is rare enough to
            // leak it instead, so a panic here can't become a double panic in
            // `RelayBody::drop`.
            std::mem::forget(payload);
            tracing::error!(
                stage = stage.as_str(),
                "response observer panicked; this call is recorded without its metadata"
            );
            None
        }
    }
}

/// Makes the observer panic at a chosen stage, on this thread (unit tests).
#[cfg(test)]
mod observer_panic_hook {
    use std::cell::Cell;

    use super::ObserverStage;

    thread_local! {
        static PANIC_AT: Cell<Option<ObserverStage>> = const { Cell::new(None) };
    }

    pub(super) fn set(stage: Option<ObserverStage>) {
        PANIC_AT.with(|cell| cell.set(stage));
    }

    pub(super) fn trigger(stage: ObserverStage) {
        if PANIC_AT.with(Cell::get) == Some(stage) {
            panic!("test observer panic at {stage:?}");
        }
    }
}

/// How the upstream body ended.
#[derive(Clone, Copy)]
enum UpstreamEnd {
    Complete,
    Failed,
}

/// The response body: relays upstream frames and records the call once, from
/// `Drop` (see the module docs).
struct RelayBody {
    upstream: Option<reqwest::Body>,
    /// The last chunk handed to hyper, not yet observed.
    unobserved: Option<Bytes>,
    relayed_bytes: u64,
    /// The body length the client expects, when known.
    expected_bytes: Option<u64>,
    ended: Option<UpstreamEnd>,
    /// The upstream failed; the next poll fails the client's body.
    abort_pending: bool,
    recording: Option<Recording>,
}

impl RelayBody {
    fn new(
        upstream: reqwest::Body,
        expected_bytes: Option<u64>,
        recording: Option<Recording>,
    ) -> Self {
        Self {
            upstream: Some(upstream),
            unobserved: None,
            relayed_bytes: 0,
            expected_bytes,
            ended: None,
            abort_pending: false,
            recording,
        }
    }

    fn observe_unobserved(&mut self) {
        let Some(chunk) = self.unobserved.take() else {
            return;
        };
        if let Some(recording) = &mut self.recording {
            recording.observer.feed(&chunk);
            if let Some(capture) = &mut recording.call.capture {
                capture.add_response(&chunk);
            }
        }
    }

    fn outcome(&self) -> CallOutcome {
        match self.ended {
            Some(UpstreamEnd::Complete) => CallOutcome::Completed,
            Some(UpstreamEnd::Failed) => CallOutcome::Incomplete,
            // hyper stops polling once a known length is written.
            None if self.expected_bytes == Some(self.relayed_bytes) => CallOutcome::Completed,
            None => CallOutcome::ClientCancelled,
        }
    }
}

impl Body for RelayBody {
    type Data = Bytes;
    type Error = UpstreamBodyFailed;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, UpstreamBodyFailed>>> {
        let this = self.get_mut();
        this.observe_unobserved();
        if this.abort_pending {
            this.abort_pending = false;
            return Poll::Ready(Some(Err(UpstreamBodyFailed)));
        }
        let Some(upstream) = &mut this.upstream else {
            return Poll::Ready(None);
        };
        match ready!(Pin::new(upstream).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let Some(chunk) = frame.data_ref() {
                    this.relayed_bytes += chunk.len() as u64;
                    this.unobserved = Some(chunk.clone());
                } else if frame.is_trailers() {
                    // The last frame; hyper stops polling after it.
                    this.upstream = None;
                    this.ended = Some(UpstreamEnd::Complete);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(error)) => {
                tracing::warn!(error = %error_chain(&error), "provider response ended with an error");
                this.upstream = None;
                this.ended = Some(UpstreamEnd::Failed);
                // Let hyper flush what it buffered this pass before it aborts.
                this.abort_pending = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            None => {
                this.upstream = None;
                this.ended = Some(UpstreamEnd::Complete);
                Poll::Ready(None)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.upstream.is_none() && !self.abort_pending
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

impl Drop for RelayBody {
    fn drop(&mut self) {
        // Drop the upstream response first, so an abandoned call closes its
        // upstream connection before anything else happens.
        self.upstream = None;
        self.observe_unobserved();
        let mut outcome = self.outcome();
        let Some(recording) = self.recording.take() else {
            return;
        };
        let observed = recording.observer.finish();
        outcome = with_upstream_error(outcome, recording.status, &observed);
        recording
            .call
            .submit(recording.status, outcome, observed, Some(recording.ttfb_ms));
    }
}

/// A body that ended normally is `completed` for a status below 400 (2xx, or
/// a 3xx passed through) and `upstreamError` when the status is 400 or more or
/// the observer saw an error (an SSE `error` event).
fn with_upstream_error(outcome: CallOutcome, status: u16, observed: &Observed) -> CallOutcome {
    if outcome == CallOutcome::Completed && (status >= 400 || observed.error_type.is_some()) {
        CallOutcome::UpstreamError
    } else {
        outcome
    }
}

/// The proxy's answer when the upstream request failed: 504 `timeout_error`
/// for a timeout (only the connect phase has one), else 502 `api_error`.
fn unreachable_reply(error: &reqwest::Error) -> (StatusCode, &'static str, &'static str) {
    if error.is_timeout() {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "timeout_error",
            "the connection timed out",
        )
    } else {
        (
            StatusCode::BAD_GATEWAY,
            "api_error",
            "the connection failed",
        )
    }
}

/// An error in the Anthropic shape:
/// `{"type":"error","error":{"type":…,"message":…}}`.
pub fn error_response(status: StatusCode, error_type: &str, message: &str) -> Response<ProxyBody> {
    json_response(status, error_body(error_type, message))
}

fn error_body(error_type: &str, message: &str) -> Bytes {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": error_type, "message": message },
    });
    Bytes::from(body.to_string())
}

fn json_response(status: StatusCode, body: Bytes) -> Response<ProxyBody> {
    let mut response = Response::new(Full::new(body).map_err(|never| match never {}).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}

/// Whether a response with this status can carry a body (RFC 9110, 6.4.1).
fn may_have_body(status: StatusCode) -> bool {
    !(status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED)
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The client's `User-Agent`, cut to at most [`MAX_USER_AGENT_BYTES`] on a
/// character boundary.
fn user_agent(headers: &HeaderMap) -> Option<String> {
    let value = String::from_utf8_lossy(headers.get(USER_AGENT)?.as_bytes()).into_owned();
    let mut end = value.len().min(MAX_USER_AGENT_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    Some(value[..end].to_owned())
}

/// An error and its causes, without the request URL.
fn error_chain(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    if let Some(url) = error.url() {
        text = text.replace(&format!(" for url ({url})"), "");
    }
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_accepts_https_and_loopback_http_only() {
        let anthropic = Upstream::parse(Upstream::ANTHROPIC).unwrap();
        assert_eq!(anthropic.as_str(), "https://api.anthropic.com");
        assert_eq!(anthropic.authority(), "api.anthropic.com");
        assert_eq!(
            Upstream::parse("https://api.anthropic.com/").unwrap(),
            anthropic
        );
        let gateway = Upstream::parse("https://gw.example:8443/prefix/").unwrap();
        assert_eq!(gateway.as_str(), "https://gw.example:8443/prefix");
        assert_eq!(gateway.authority(), "gw.example:8443");

        let v4 = Upstream::parse("http://127.0.0.1:9000").unwrap();
        assert_eq!(v4.authority(), "127.0.0.1:9000");
        assert!(Upstream::parse("http://127.9.9.9:9000").is_ok());
        assert!(Upstream::parse("http://[::1]:9000").is_ok());

        for refused in [
            "http://localhost:9000",
            "http://api.anthropic.com",
            "http://10.0.0.1:9000",
            "ftp://127.0.0.1",
            "https://api.anthropic.com?x=1",
            "https://api.anthropic.com#frag",
            "not a url",
            "",
        ] {
            assert!(Upstream::parse(refused).is_err(), "{refused}");
        }
        let error = Upstream::parse("https://user:hunter2@api.anthropic.com").unwrap_err();
        assert!(!error.to_string().contains("hunter2"), "{error}");
    }

    #[test]
    fn url_for_keeps_the_path_and_query_or_refuses() {
        let upstream = Upstream::parse("http://127.0.0.1:9000").unwrap();
        assert_eq!(
            upstream.url_for("/v1/messages?beta=true").unwrap().as_str(),
            "http://127.0.0.1:9000/v1/messages?beta=true"
        );
        assert!(upstream.url_for("/v1/models?limit=1000&").is_some());
        assert!(upstream.url_for("/v1/x?").is_some());
        // The URL parser would resolve these; forwarding them changed is refused.
        assert!(upstream.url_for("/v1/../admin").is_none());
        assert!(upstream.url_for("/v1/./messages").is_none());
        assert!(upstream.url_for("/v1/%2e%2e/admin").is_none());
        // It only percent-encodes these, which means the same.
        assert_eq!(
            upstream
                .url_for("/v1/models?after_id='a'")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:9000/v1/models?after_id=%27a%27"
        );
        assert_eq!(
            upstream.url_for("/v1/x/{id}").unwrap().as_str(),
            "http://127.0.0.1:9000/v1/x/%7Bid%7D"
        );
        assert!(upstream.url_for("/v1/x%2Fy?q=%zz").is_some());

        assert_eq!(percent_decoded(b"a%27b%2fc%zz%4"), b"a'b/c%zz%4");

        let prefixed = Upstream::parse("https://gw.example/prefix").unwrap();
        assert_eq!(
            prefixed.url_for("/v1/messages").unwrap().as_str(),
            "https://gw.example/prefix/v1/messages"
        );
    }

    #[test]
    fn a_normal_end_is_an_upstream_error_on_4xx_5xx_or_an_observed_error() {
        let clean = Observed::default();
        let errored = Observed {
            error_type: Some("overloaded_error".into()),
            ..Observed::default()
        };
        let done = CallOutcome::Completed;
        assert_eq!(with_upstream_error(done, 200, &clean), done);
        assert_eq!(with_upstream_error(done, 307, &clean), done);
        assert_eq!(
            with_upstream_error(done, 200, &errored),
            CallOutcome::UpstreamError
        );
        assert_eq!(
            with_upstream_error(done, 400, &clean),
            CallOutcome::UpstreamError
        );
        assert_eq!(
            with_upstream_error(done, 529, &clean),
            CallOutcome::UpstreamError
        );
        // A call that didn't end normally keeps its outcome.
        for other in [CallOutcome::ClientCancelled, CallOutcome::Incomplete] {
            assert_eq!(with_upstream_error(other, 500, &errored), other);
        }
    }

    #[test]
    fn capture_keeps_both_bodies_within_the_cap_and_none_over_it() {
        let mut fits = Capture::new(Bytes::from_static(b"req"));
        fits.add_response(b"res");
        fits.add_response(b"ponse");
        assert_eq!(
            fits.into_content(),
            (vec![b"req".to_vec(), b"response".to_vec()], None)
        );

        let mut exactly = Capture::new(Bytes::from(vec![1; MAX_CONTENT_BYTES - 1]));
        exactly.add_response(b"x");
        let (content, truncated) = exactly.into_content();
        assert_eq!((content.len(), truncated), (2, None));

        let mut response_over = Capture::new(Bytes::from(vec![1; MAX_CONTENT_BYTES]));
        response_over.add_response(b"x");
        assert!(response_over.response.is_empty() && response_over.request.is_empty());
        response_over.add_response(b"y");
        assert_eq!(response_over.into_content(), (Vec::new(), Some(true)));

        let request_over = Capture::new(Bytes::from(vec![1; MAX_CONTENT_BYTES + 1]));
        assert!(request_over.request.is_empty());
        assert_eq!(request_over.into_content(), (Vec::new(), Some(true)));
    }

    #[test]
    fn user_agent_is_cut_to_200_bytes_on_a_character_boundary() {
        let mut headers = HeaderMap::new();
        assert_eq!(user_agent(&headers), None);
        headers.insert(
            USER_AGENT,
            "claude-cli/2.1.300 (external, cli)".parse().unwrap(),
        );
        assert_eq!(
            user_agent(&headers).as_deref(),
            Some("claude-cli/2.1.300 (external, cli)")
        );
        // 199 ASCII bytes, then a 2-byte character that would end at byte 201.
        let long = format!("{}é tail", "a".repeat(199));
        headers.insert(
            USER_AGENT,
            http::HeaderValue::from_bytes(long.as_bytes()).unwrap(),
        );
        assert_eq!(user_agent(&headers).unwrap(), "a".repeat(199));
        headers.insert(USER_AGENT, "b".repeat(300).parse().unwrap());
        assert_eq!(user_agent(&headers).unwrap().len(), MAX_USER_AGENT_BYTES);
    }

    #[tokio::test]
    async fn a_refused_connection_is_a_502() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let upstream = Upstream::parse(&format!("http://{addr}")).unwrap();
        let client = build_client(&upstream, CONNECT_TIMEOUT).unwrap();
        let error = client
            .get(format!("http://{addr}/v1/models"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(
            unreachable_reply(&error),
            (
                StatusCode::BAD_GATEWAY,
                "api_error",
                "the connection failed"
            )
        );
        assert!(!error_chain(&error).contains("/v1/models"));
    }

    /// A listener whose accept queue is full drops new SYNs (Linux), so
    /// connecting to it hangs until the client's connect timeout. The outer
    /// timeout keeps a kernel that answers anyway from hanging the suite.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_connect_timeout_is_a_504() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
            let listener = socket.listen(1).unwrap();
            let addr = listener.local_addr().unwrap();
            let mut queued = Vec::new();
            for _ in 0..64 {
                let connect = tokio::net::TcpStream::connect(addr);
                match tokio::time::timeout(Duration::from_millis(200), connect).await {
                    Ok(Ok(stream)) => queued.push(stream),
                    _ => break,
                }
            }

            let upstream = Upstream::parse(&format!("http://{addr}")).unwrap();
            let timeout = Duration::from_millis(300);
            let client = build_client(&upstream, timeout).unwrap();
            let started = Instant::now();
            let error = client
                .get(format!("http://{addr}/v1/models"))
                .send()
                .await
                .unwrap_err();
            assert!(started.elapsed() >= timeout);
            assert!(error.is_connect(), "{}", error_chain(&error));
            assert_eq!(
                unreachable_reply(&error),
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    "timeout_error",
                    "the connection timed out"
                )
            );
            drop(queued);
        })
        .await
        .expect("the connect-timeout test hung");
    }

    /// Over HTTP/2 a DATA frame and a stream reset can be ready together, so
    /// the relay sees a chunk and the failure in one hyper write pass. The
    /// chunk (and the head) must still reach the client, then the connection
    /// closes with no last chunk. reqwest's HTTP/1 client can't produce this
    /// (it reads on only after the body asks), hence a unit test.
    #[tokio::test]
    async fn a_chunk_and_a_failure_ready_together_reach_the_client_as_chunk_then_abort() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(|_request: Request<Incoming>| async {
                let upstream = reqwest::Body::wrap_stream(futures_util::stream::iter(vec![
                    Ok(Bytes::from_static(b"hello")),
                    Err(std::io::Error::other("stream reset")),
                ]));
                let body = RelayBody::new(upstream, None, None);
                Ok::<_, std::convert::Infallible>(Response::new(BoxBody::new(body)))
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .await;
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.writable().await.unwrap();
        stream
            .try_write(b"GET /v1/messages HTTP/1.1\r\nhost: x\r\n\r\n")
            .unwrap();
        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut chunk = [0u8; 4096];
            loop {
                stream.readable().await.unwrap();
                match stream.try_read(&mut chunk) {
                    Ok(0) => return,
                    Ok(n) => received.extend_from_slice(&chunk[..n]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    // A reset after the data also ends the response.
                    Err(_) => return,
                }
            }
        })
        .await
        .expect("the connection stayed open");
        let text = String::from_utf8_lossy(&received);
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text:?}");
        assert!(
            text.ends_with("\r\n\r\n5\r\nhello\r\n"),
            "the chunk, then nothing: {text:?}"
        );
    }

    /// A sink that keeps every call (unit tests).
    #[derive(Default)]
    struct KeepCalls(std::sync::Mutex<Vec<PendingCall>>);

    impl CallSink for KeepCalls {
        fn submit(&self, call: PendingCall) {
            self.0.lock().unwrap().push(call);
        }
    }

    /// A recorded relay over `chunks`, as `handle` builds it.
    fn recorded_relay(sink: &Arc<KeepCalls>, chunks: &[&'static [u8]]) -> RelayBody {
        let stream = futures_util::stream::iter(
            chunks
                .iter()
                .map(|chunk| Ok::<_, std::io::Error>(Bytes::from_static(chunk)))
                .collect::<Vec<_>>(),
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("text/event-stream"),
        );
        let call = CallStart {
            sink: Arc::clone(sink) as Arc<dyn CallSink>,
            run_id: "r".into(),
            method: "POST".into(),
            path: "/v1/messages".into(),
            started: Instant::now(),
            started_at_ms: unix_ms(),
            request_bytes: 2,
            trace_id: "t".into(),
            user_agent: None,
            capture: None,
        };
        let recording = Recording {
            observer: GuardedObserver::new(StatusCode::OK, &headers),
            status: 200,
            ttfb_ms: 0,
            call,
        };
        RelayBody::new(reqwest::Body::wrap_stream(stream), None, Some(recording))
    }

    #[tokio::test]
    async fn an_observer_panic_neither_breaks_the_stream_nor_loses_the_record() {
        let chunks: &[&'static [u8]] = &[b"event: a\n\n", b"event: b\n\n", b"event: c\n\n"];
        let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
        for stage in [
            ObserverStage::Start,
            ObserverStage::Feed,
            ObserverStage::Finish,
        ] {
            let sink = Arc::new(KeepCalls::default());
            observer_panic_hook::set(Some(stage));
            let relay = recorded_relay(&sink, chunks);
            let body = relay.collect().await.expect("the client's body failed");
            observer_panic_hook::set(None);
            assert_eq!(body.to_bytes(), chunks.concat(), "{stage:?}");

            let calls = sink.0.lock().unwrap();
            assert_eq!(calls.len(), 1, "{stage:?}: recorded once");
            let record = &calls[0].record;
            assert_eq!(record.outcome, CallOutcome::Completed, "{stage:?}");
            assert_eq!(record.status, 200, "{stage:?}");
            assert_eq!(record.response_bytes, total as u64, "{stage:?}");
            // The observer's own findings are gone (the stub's `streamed`).
            assert!(!record.streamed, "{stage:?}");
        }

        // Without a panic the observer's findings are kept.
        let sink = Arc::new(KeepCalls::default());
        recorded_relay(&sink, chunks).collect().await.unwrap();
        assert!(sink.0.lock().unwrap()[0].record.streamed);
    }
}
