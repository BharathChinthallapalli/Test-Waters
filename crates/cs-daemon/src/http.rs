//! The HTTP layer of the control API (R2.1, R2.3, R2.4).
//!
//! Owned by unit `rpc-http`: one route, `POST /rpc`, with layers outermost first:
//! 64 KiB body limit, 408 after 10 s, 403 for any `Origin` header or a `Host` other
//! than `127.0.0.1:<port>`, 401 for a missing or wrong bearer token, then dispatch.
//! Header values are never logged.
//!
//! The layers wrap every path, so a request to any other path or with another method
//! is still checked for Host, Origin and token before it gets 404 or 405.
//!
//! Serve the router with `into_make_service_with_connect_info::<SocketAddr>()` so
//! auth failures can name the peer; without it the peer is logged as unknown.
//!
//! **Header-read timeout.** `axum::serve` in axum 0.8.9 builds a fresh
//! `hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())` for every
//! connection and exposes no way to configure it (`axum/src/serve/mod.rs`,
//! `handle_connection`). hyper's HTTP/1 `header_read_timeout` defaults to 30 s but
//! only takes effect with a timer, and that builder sets none, so hyper drops the
//! default (`hyper/src/server/conn/http1.rs`, `Builder::new` sets `Time::Empty`;
//! `hyper/src/common/time.rs`, `Time::check`). The 10 s timeout here starts once the
//! headers have been read; it covers reading the body and the dispatch.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tower_http::timeout::TimeoutLayer;

use crate::rpc::{self, Handler, Reply};

/// The only route of the control API.
pub const RPC_PATH: &str = "/rpc";

/// Largest accepted request body; bigger bodies get 413.
pub const BODY_LIMIT_BYTES: usize = 64 * 1024;

/// A request that hasn't been answered after this long gets 408.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Checks a presented bearer token. Unit `daemon-proc` implements it with the
/// rotating control token; [`StaticToken`] is a fixed one for tests and examples.
///
/// Implementations compare in constant time, for example with [`tokens_match`].
pub trait TokenVerifier: Send + Sync + 'static {
    fn verify(&self, presented: &[u8]) -> bool;
}

/// Compares `presented` with `expected` in constant time. Only the length, which is
/// public, can end the comparison early.
pub fn tokens_match(expected: &[u8], presented: &[u8]) -> bool {
    expected.len() == presented.len() && bool::from(expected.ct_eq(presented))
}

/// A token that never changes. `Debug` doesn't print it.
pub struct StaticToken(Vec<u8>);

impl StaticToken {
    pub fn new(token: impl Into<Vec<u8>>) -> Self {
        Self(token.into())
    }
}

impl std::fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StaticToken(<redacted>)")
    }
}

impl TokenVerifier for StaticToken {
    fn verify(&self, presented: &[u8]) -> bool {
        tokens_match(&self.0, presented)
    }
}

/// Settings of the HTTP layer that differ between the daemon and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpConfig {
    /// The port the listener is actually bound to; the only accepted `Host` is
    /// `127.0.0.1:<port>`. Never the configured port, which may be 0: use
    /// [`HttpConfig::for_listener`].
    pub port: u16,
    /// [`REQUEST_TIMEOUT`] in the daemon; tests shorten it.
    pub request_timeout: Duration,
}

impl HttpConfig {
    /// The daemon's settings for a listener bound to `port`.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            request_timeout: REQUEST_TIMEOUT,
        }
    }

    /// The daemon's settings for `listener`, with the port it is bound to.
    pub fn for_listener(listener: &TcpListener) -> io::Result<Self> {
        Ok(Self::new(listener.local_addr()?.port()))
    }
}

/// Builds the control API's router: `POST /rpc` behind the body limit, the timeout,
/// the Host/Origin check and the bearer check, in that order from the outside.
pub fn router<V: TokenVerifier, H: Handler>(
    config: HttpConfig,
    verifier: Arc<V>,
    handler: Arc<H>,
) -> Router {
    let expected_host = ExpectedHost(Arc::from(format!("127.0.0.1:{}", config.port)));
    // `Router::layer` wraps what is already there, so the last layer is outermost.
    Router::new()
        .route(RPC_PATH, post(rpc_endpoint::<H>))
        .with_state(handler)
        .layer(middleware::from_fn_with_state(
            verifier,
            require_bearer::<V>,
        ))
        .layer(middleware::from_fn_with_state(
            expected_host,
            require_local_host,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            config.request_timeout,
        ))
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
}

/// `127.0.0.1:<port>`, the only accepted `Host` value.
#[derive(Clone)]
struct ExpectedHost(Arc<str>);

/// 403 when an `Origin` header is present or `Host` isn't exactly the bound address.
async fn require_local_host(
    State(expected): State<ExpectedHost>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if headers.contains_key(header::ORIGIN) || !host_is(headers, &expected.0) {
        tracing::warn!(
            peer = %peer(&request),
            "control API request rejected: Origin present or Host not the bound address"
        );
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

/// 401 when `Authorization: Bearer <token>` is missing or the token is wrong.
async fn require_bearer<V: TokenVerifier>(
    State(verifier): State<Arc<V>>,
    request: Request,
    next: Next,
) -> Response {
    let authorized =
        bearer_token(request.headers()).is_some_and(|presented| verifier.verify(presented));
    if !authorized {
        tracing::warn!(
            peer = %peer(&request),
            "control API request rejected: missing or wrong bearer token"
        );
        return unauthorized();
    }
    next.run(request).await
}

async fn rpc_endpoint<H: Handler>(State(handler): State<Arc<H>>, body: Bytes) -> Response {
    match rpc::dispatch(handler.as_ref(), &body).await {
        Reply::Single(response) => axum::Json(response).into_response(),
        Reply::Batch(responses) => axum::Json(responses).into_response(),
        Reply::Nothing => StatusCode::NO_CONTENT.into_response(),
    }
}

/// True when there is exactly one `Host` header and it equals `expected`.
fn host_is(headers: &HeaderMap, expected: &str) -> bool {
    let mut hosts = headers.get_all(header::HOST).iter();
    match (hosts.next(), hosts.next()) {
        (Some(host), None) => host.as_bytes() == expected.as_bytes(),
        _ => false,
    }
}

/// The token of the only `Authorization` header, if it uses the Bearer scheme:
/// `"Bearer" 1*SP token` (RFC 6750, section 2.1), with the scheme name matched
/// case-insensitively (RFC 9110, section 11.1).
fn bearer_token(headers: &HeaderMap) -> Option<&[u8]> {
    const SCHEME: &[u8] = b"bearer";
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return None;
    };
    let (scheme, rest) = value.as_bytes().split_at_checked(SCHEME.len())?;
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return None;
    }
    let spaces = rest.iter().take_while(|&&byte| byte == b' ').count();
    if spaces == 0 {
        return None;
    }
    rest.get(spaces..)
}

/// 401 with the challenge RFC 9110 (section 15.5.2) requires, and no body.
fn unauthorized() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

/// The peer's address for logs, when the server recorded it.
fn peer(request: &Request) -> String {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "unknown".to_owned(), |info| info.0.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::RpcError;
    use crate::rpc::tests::EchoHandler;
    use axum::body::to_bytes;
    use serde_json::{Value, json};
    use std::io::Write;
    use std::sync::Mutex;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const PORT: u16 = 41234;

    fn app() -> Router {
        router(
            HttpConfig::new(PORT),
            Arc::new(StaticToken::new(TOKEN)),
            Arc::new(EchoHandler::default()),
        )
    }

    fn request() -> axum::http::request::Builder {
        Request::builder()
            .method("POST")
            .uri(RPC_PATH)
            .header(header::HOST, format!("127.0.0.1:{PORT}"))
            .header(header::CONTENT_TYPE, "application/json")
    }

    fn authorized(body: impl Into<Body>) -> Request {
        request()
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(body.into())
            .unwrap()
    }

    async fn send(app: Router, request: Request) -> (StatusCode, Bytes) {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        (
            status,
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        )
    }

    async fn send_json(request: Request) -> (StatusCode, Value) {
        let (status, body) = send(app(), request).await;
        (status, serde_json::from_slice(&body).unwrap())
    }

    const CALL: &str = r#"{"jsonrpc":"2.0","method":"echo","params":{"ok":true},"id":1}"#;

    #[test]
    fn tokens_match_only_on_equal_bytes() {
        assert!(tokens_match(b"abc", b"abc"));
        assert!(!tokens_match(b"abc", b"abd"));
        assert!(!tokens_match(b"abc", b"ab"));
        assert!(!tokens_match(b"abc", b"abcd"));
        assert!(!tokens_match(b"abc", b""));
    }

    #[test]
    fn static_token_debug_hides_the_token() {
        assert!(!format!("{:?}", StaticToken::new(TOKEN)).contains(TOKEN));
    }

    #[tokio::test]
    async fn successful_call_returns_json_rpc_response() {
        let (status, body) = send_json(authorized(CALL)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": 1 })
        );
    }

    #[tokio::test]
    async fn response_is_json() {
        let response = app().oneshot(authorized(CALL)).await.unwrap();

        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    }

    #[tokio::test]
    async fn missing_token_is_401_with_challenge() {
        let request = request().body(Body::from(CALL)).unwrap();

        let response = app().oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn wrong_token_is_401() {
        let wrong = [
            format!("Bearer {}", &TOKEN[1..]),
            format!("Bearer {TOKEN}0"),
            format!("Bearer {}", TOKEN.replace('0', "1")),
            format!("Basic {TOKEN}"),
            format!("Bearer\t{TOKEN}"),
            format!("Bearer{TOKEN}"),
            format!("Bearer {TOKEN} "),
            TOKEN.to_owned(),
            "Bearer ".to_owned(),
            "Bearer".to_owned(),
        ];
        for value in wrong {
            let request = request()
                .header(header::AUTHORIZATION, &value)
                .body(Body::from(CALL))
                .unwrap();

            let (status, _) = send(app(), request).await;

            assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
        }
    }

    #[tokio::test]
    async fn two_authorization_headers_are_401() {
        let request = request()
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::from(CALL))
            .unwrap();

        assert_eq!(send(app(), request).await.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_scheme_is_case_insensitive_and_takes_one_or_more_spaces() {
        for value in [
            format!("bearer {TOKEN}"),
            format!("BEARER {TOKEN}"),
            format!("Bearer   {TOKEN}"),
        ] {
            let request = request()
                .header(header::AUTHORIZATION, &value)
                .body(Body::from(CALL))
                .unwrap();

            assert_eq!(send(app(), request).await.0, StatusCode::OK, "{value}");
        }
    }

    #[tokio::test]
    async fn origin_header_is_403_even_with_a_valid_token() {
        for origin in ["http://evil.example", "null", "http://127.0.0.1:41234"] {
            let mut request = authorized(CALL);
            request
                .headers_mut()
                .insert(header::ORIGIN, HeaderValue::from_static(origin));

            assert_eq!(
                send(app(), request).await.0,
                StatusCode::FORBIDDEN,
                "{origin}"
            );
        }
    }

    #[tokio::test]
    async fn wrong_host_is_403() {
        let hosts = [
            format!("localhost:{PORT}"),
            "127.0.0.1:1".to_owned(),
            "127.0.0.1".to_owned(),
            format!("[::1]:{PORT}"),
            format!("127.0.0.1:{PORT}."),
            format!("127.0.0.01:{PORT}"),
        ];
        for host in hosts {
            let mut request = authorized(CALL);
            request
                .headers_mut()
                .insert(header::HOST, HeaderValue::from_str(&host).unwrap());

            assert_eq!(
                send(app(), request).await.0,
                StatusCode::FORBIDDEN,
                "{host}"
            );
        }
    }

    #[tokio::test]
    async fn missing_or_repeated_host_is_403() {
        let mut missing = authorized(CALL);
        missing.headers_mut().remove(header::HOST);
        let mut repeated = authorized(CALL);
        repeated.headers_mut().append(
            header::HOST,
            HeaderValue::from_str(&format!("127.0.0.1:{PORT}")).unwrap(),
        );

        assert_eq!(send(app(), missing).await.0, StatusCode::FORBIDDEN);
        assert_eq!(send(app(), repeated).await.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn host_check_comes_before_the_token_check() {
        let request = request()
            .header(header::ORIGIN, "http://evil.example")
            .body(Body::from(CALL))
            .unwrap();

        assert_eq!(send(app(), request).await.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn other_paths_and_methods_are_checked_too() {
        let unauthenticated = request().uri("/other").body(Body::empty()).unwrap();
        let get = request()
            .method("GET")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let unknown_path = request()
            .uri("/other")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();

        assert_eq!(
            send(app(), unauthenticated).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(send(app(), get).await.0, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(send(app(), unknown_path).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn body_over_64_kib_is_413() {
        let padding = "x".repeat(BODY_LIMIT_BYTES);
        let body = format!(r#"{{"jsonrpc":"2.0","method":"echo","params":["{padding}"],"id":1}}"#);

        let (status, _) = send(app(), authorized(body)).await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn body_of_exactly_64_kib_is_accepted() {
        let prefix = r#"{"jsonrpc":"2.0","method":"echo","params":[""#;
        let suffix = r#""],"id":1}"#;
        let padding = "x".repeat(BODY_LIMIT_BYTES - prefix.len() - suffix.len());
        let body = format!("{prefix}{padding}{suffix}");
        assert_eq!(body.len(), BODY_LIMIT_BYTES);

        assert_eq!(send(app(), authorized(body)).await.0, StatusCode::OK);
    }

    /// Sleeps longer than any test timeout before answering.
    struct SlowHandler;

    impl Handler for SlowHandler {
        async fn call(&self, _method: &str, _params: Option<Value>) -> Result<Value, RpcError> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(Value::Null)
        }
    }

    #[tokio::test]
    async fn slow_request_is_408() {
        // tokio's `test-util` (for `time::pause`) isn't enabled, so the timeout is
        // shortened instead.
        let config = HttpConfig {
            port: PORT,
            request_timeout: Duration::from_millis(50),
        };
        let app = router(
            config,
            Arc::new(StaticToken::new(TOKEN)),
            Arc::new(SlowHandler),
        );

        let (status, body) = send(app, authorized(CALL)).await;

        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert!(body.is_empty());
    }

    #[test]
    fn daemon_timeout_is_ten_seconds() {
        assert_eq!(
            HttpConfig::new(PORT).request_timeout,
            Duration::from_secs(10)
        );
    }

    #[tokio::test]
    async fn malformed_json_is_parse_error() {
        let (status, body) = send_json(authorized("{")).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({ "jsonrpc": "2.0", "error": { "code": -32700, "message": "Parse error" }, "id": null })
        );
    }

    #[tokio::test]
    async fn invalid_request_is_32600() {
        let (_, body) = send_json(authorized(r#"{"jsonrpc":"1.0","method":"echo","id":4}"#)).await;

        assert_eq!(body["error"]["code"], -32600);
        assert_eq!(body["id"], 4);
    }

    #[tokio::test]
    async fn unknown_method_is_32601() {
        let (_, body) = send_json(authorized(r#"{"jsonrpc":"2.0","method":"nope","id":5}"#)).await;

        assert_eq!(body["error"]["code"], -32601);
        assert_eq!(body["id"], 5);
    }

    #[tokio::test]
    async fn single_notification_is_204_without_body() {
        let (status, body) = send(app(), authorized(r#"{"jsonrpc":"2.0","method":"echo"}"#)).await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn batch_of_only_notifications_is_204_without_body() {
        let batch = r#"[{"jsonrpc":"2.0","method":"echo"},{"jsonrpc":"2.0","method":"nope"}]"#;

        let (status, body) = send(app(), authorized(batch)).await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn mixed_batch_answers_calls_in_order() {
        let batch = r#"[
            {"jsonrpc":"2.0","method":"echo","params":[1],"id":1},
            {"jsonrpc":"2.0","method":"echo","params":[2]},
            {"jsonrpc":"2.0","method":"nope","id":3}
        ]"#;

        let (status, body) = send_json(authorized(batch)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!([
                { "jsonrpc": "2.0", "result": [1], "id": 1 },
                { "jsonrpc": "2.0", "error": { "code": -32601, "message": "Method not found" }, "id": 3 },
            ])
        );
    }

    #[tokio::test]
    async fn empty_and_oversized_batches_are_one_32600() {
        let entry = json!({ "jsonrpc": "2.0", "method": "echo", "id": 1 });
        let oversized = Value::Array(vec![entry; cs_core::rpc::MAX_BATCH_LEN + 1]).to_string();

        for batch in ["[]".to_owned(), oversized] {
            let (status, body) = send_json(authorized(batch)).await;

            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                body,
                json!({ "jsonrpc": "2.0", "error": { "code": -32600, "message": "Invalid Request" }, "id": null })
            );
        }
    }

    /// Collects everything the subscriber writes.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn auth_failures_are_logged_without_header_values() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let wrong_token = "presented-but-wrong-9f8e7d6c5b4a";
        let origin = "http://origin-value.example";

        let wrong = request()
            .header(header::AUTHORIZATION, format!("Bearer {wrong_token}"))
            .body(Body::from(CALL))
            .unwrap();
        let with_origin = request()
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::ORIGIN, origin)
            .body(Body::from(CALL))
            .unwrap();
        assert_eq!(send(app(), wrong).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(send(app(), with_origin).await.0, StatusCode::FORBIDDEN);
        assert_eq!(send(app(), authorized(CALL)).await.0, StatusCode::OK);

        let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("missing or wrong bearer token"), "{logs}");
        assert!(logs.contains("Origin present"), "{logs}");
        for secret in [TOKEN, wrong_token, origin, "echo"] {
            assert!(!logs.contains(secret), "log contains {secret}: {logs}");
        }
    }
}
