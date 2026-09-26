//! JSON-RPC 2.0 parsing, dispatch and batches (R2.1, R2.6).
//!
//! Owned by unit `rpc-http`: parses raw JSON so malformed requests get -32700 or
//! -32600; dispatches through a handler trait; batches of 1 to 16 in order, with no
//! entries for notifications, HTTP 204 for a batch of only notifications, and one
//! -32600 with `id: null` for an empty array or more than 16 entries.
//!
//! The body is parsed as a [`serde_json::Value`] first, never straight into a typed
//! request, so every malformed input still gets the matching error:
//! - not JSON → -32700 with `id: null`;
//! - not a request object (`jsonrpc` other than `"2.0"`, `method` not a string, `id`
//!   not a string, `null` or an integer within ±(2^53 − 1), `params` present but not
//!   an object or array) → -32600, with the request's `id` if it could be read and
//!   `null` otherwise;
//! - everything else goes to the [`Handler`], which reports -32601 for an unknown
//!   method, -32602 for bad params, or a Callsheet code (`cs_core::rpc::codes`).
//!
//! A request without an `id` member is a notification: it runs, and nothing is
//! returned for it. Error messages are fixed strings and never echo the request.

use std::future::Future;

use cs_core::rpc::{ErrorObject, JSONRPC_VERSION, MAX_BATCH_LEN, RequestId, Response, codes};
use serde_json::{Map, Value};

/// The control-API methods, as seen by the dispatcher. Unit `wire` implements it.
///
/// The dispatcher has already checked the envelope, so `method` is a string and
/// `params` is an object or array when present. Implementations answer an unknown
/// method with [`RpcError::method_not_found`] and params that don't fit with
/// [`RpcError::invalid_params`]. The HTTP layer keeps the handler in an `Arc`
/// (axum state), so it is shared by every request.
///
/// **Cancellation.** Every call in one HTTP request, a whole batch included, shares
/// the request's timeout (10 s). When it fires, the dispatch future is dropped at
/// its current `.await`: the call in progress stops there, and the client gets a
/// 408 without learning which calls already took effect. So a call must never be
/// left half-done by being dropped: work that has to finish (a store write, a
/// token rotation) is handed to something that outlives the request, such as the
/// store's writer thread, and the future only awaits its reply.
pub trait Handler: Send + Sync + 'static {
    fn call(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> impl Future<Output = Result<Value, RpcError>> + Send;
}

/// An error a [`Handler`] returns; it becomes the response's `error` member.
///
/// `message` goes to the client as is, so it must never contain request contents,
/// tokens or content bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// -32601: the method does not exist.
    pub fn method_not_found() -> Self {
        Self::new(codes::METHOD_NOT_FOUND, "Method not found")
    }

    /// -32602: the method exists but its params don't fit.
    pub fn invalid_params() -> Self {
        Self::new(codes::INVALID_PARAMS, "Invalid params")
    }

    /// -32603: the daemon failed in a way the client can't fix.
    pub fn internal_error() -> Self {
        Self::new(codes::INTERNAL_ERROR, "Internal error")
    }

    fn parse_error() -> Self {
        Self::new(codes::PARSE_ERROR, "Parse error")
    }

    fn invalid_request() -> Self {
        Self::new(codes::INVALID_REQUEST, "Invalid Request")
    }
}

impl From<RpcError> for ErrorObject {
    fn from(error: RpcError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

/// What to send back for one HTTP request body.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// A single response object: one call, or an error about the whole body.
    Single(Response<Value>),
    /// A non-empty array, one entry per call in the batch, in request order.
    Batch(Vec<Response<Value>>),
    /// Nothing: a notification, or a batch of only notifications (HTTP 204).
    Nothing,
}

/// Parses `body` and runs every call in it through `handler`, in order.
pub async fn dispatch<H: Handler>(handler: &H, body: &[u8]) -> Reply {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Reply::Single(error_response(None, RpcError::parse_error()));
    };
    match value {
        Value::Array(entries) => dispatch_batch(handler, entries).await,
        single => match dispatch_entry(handler, single).await {
            Some(response) => Reply::Single(response),
            None => Reply::Nothing,
        },
    }
}

async fn dispatch_batch<H: Handler>(handler: &H, entries: Vec<Value>) -> Reply {
    if entries.is_empty() || entries.len() > MAX_BATCH_LEN {
        return Reply::Single(error_response(None, RpcError::invalid_request()));
    }
    let mut responses = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(response) = dispatch_entry(handler, entry).await {
            responses.push(response);
        }
    }
    if responses.is_empty() {
        Reply::Nothing
    } else {
        Reply::Batch(responses)
    }
}

/// Runs one request object; `None` for a notification.
async fn dispatch_entry<H: Handler>(handler: &H, entry: Value) -> Option<Response<Value>> {
    match parse_entry(entry) {
        Entry::Call { id, method, params } => {
            let outcome = handler.call(&method, params).await;
            Some(match outcome {
                Ok(result) => success_response(id, result),
                Err(error) => error_response(id, error),
            })
        }
        Entry::Notification { method, params } => {
            if let Err(error) = handler.call(&method, params).await {
                // Nobody receives a notification's error; the code is enough to debug.
                tracing::debug!(code = error.code, "notification failed");
            }
            None
        }
        Entry::Invalid { id } => Some(error_response(id, RpcError::invalid_request())),
    }
}

/// One element of a request body after the envelope checks.
#[derive(Debug, PartialEq)]
enum Entry {
    /// `id` is `None` when the client sent `"id": null`.
    Call {
        id: Option<RequestId>,
        method: String,
        params: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    /// Not a valid request; `id` is the request's own when it could be read.
    Invalid { id: Option<RequestId> },
}

/// The `id` member of a request object.
enum IdMember {
    Absent,
    Valid(Option<RequestId>),
    Unreadable,
}

fn parse_entry(entry: Value) -> Entry {
    let Value::Object(mut object) = entry else {
        return Entry::Invalid { id: None };
    };
    let id = read_id(&object);
    let invalid = |id: IdMember| Entry::Invalid {
        id: readable_id(id),
    };
    if !has_supported_version(&object) {
        return invalid(id);
    }
    let Some(Value::String(method)) = object.remove("method") else {
        return invalid(id);
    };
    let params = match read_params(object.remove("params")) {
        Params::Absent => None,
        Params::Structured(params) => Some(params),
        Params::Invalid => return invalid(id),
    };
    match id {
        IdMember::Absent => Entry::Notification { method, params },
        IdMember::Valid(id) => Entry::Call { id, method, params },
        IdMember::Unreadable => Entry::Invalid { id: None },
    }
}

fn has_supported_version(object: &Map<String, Value>) -> bool {
    matches!(object.get("jsonrpc"), Some(Value::String(version)) if version == JSONRPC_VERSION)
}

/// The largest id magnitude accepted, so every numeric id is exact in JavaScript
/// (`Number.MAX_SAFE_INTEGER`, see `cs_core::rpc::RequestId`).
const MAX_SAFE_ID: i64 = (1 << 53) - 1;

fn read_id(object: &Map<String, Value>) -> IdMember {
    match object.get("id") {
        None => IdMember::Absent,
        Some(Value::Null) => IdMember::Valid(None),
        Some(Value::String(id)) => IdMember::Valid(Some(RequestId::String(id.clone()))),
        // Fractions and integers a JavaScript client can't hold exactly can't be
        // echoed back faithfully, so the request is invalid.
        Some(Value::Number(number)) => match number.as_i64() {
            Some(id) if (-MAX_SAFE_ID..=MAX_SAFE_ID).contains(&id) => {
                IdMember::Valid(Some(RequestId::Number(id)))
            }
            _ => IdMember::Unreadable,
        },
        Some(_) => IdMember::Unreadable,
    }
}

fn readable_id(id: IdMember) -> Option<RequestId> {
    match id {
        IdMember::Valid(id) => id,
        IdMember::Absent | IdMember::Unreadable => None,
    }
}

/// The `params` member of a request object.
enum Params {
    Absent,
    /// An object or an array.
    Structured(Value),
    /// Present but neither an object nor an array (`null` included).
    Invalid,
}

fn read_params(params: Option<Value>) -> Params {
    match params {
        None => Params::Absent,
        Some(params @ (Value::Object(_) | Value::Array(_))) => Params::Structured(params),
        Some(_) => Params::Invalid,
    }
}

fn success_response(id: Option<RequestId>, result: Value) -> Response<Value> {
    Response {
        jsonrpc: JSONRPC_VERSION.to_owned(),
        result: Some(result),
        error: None,
        id,
    }
}

fn error_response(id: Option<RequestId>, error: RpcError) -> Response<Value> {
    Response {
        jsonrpc: JSONRPC_VERSION.to_owned(),
        result: None,
        error: Some(error.into()),
        id,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// Answers `echo` with its params, fails `fail` with 1003, and records every call.
    #[derive(Default)]
    pub(crate) struct EchoHandler {
        pub(crate) calls: Mutex<Vec<String>>,
    }

    impl Handler for EchoHandler {
        async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
            self.calls.lock().unwrap().push(method.to_owned());
            match method {
                "echo" => Ok(params.unwrap_or(Value::Null)),
                "needsObject" => match params {
                    Some(Value::Object(_)) => Ok(json!({})),
                    _ => Err(RpcError::invalid_params()),
                },
                "fail" => Err(RpcError::new(codes::UNKNOWN_RUN, "Unknown run")),
                _ => Err(RpcError::method_not_found()),
            }
        }
    }

    async fn run(body: &str) -> Reply {
        dispatch(&EchoHandler::default(), body.as_bytes()).await
    }

    fn to_json(reply: Reply) -> Value {
        match reply {
            Reply::Single(response) => serde_json::to_value(response).unwrap(),
            Reply::Batch(responses) => serde_json::to_value(responses).unwrap(),
            Reply::Nothing => panic!("expected a response"),
        }
    }

    fn error(code: i32, message: &str, id: Value) -> Value {
        json!({ "jsonrpc": "2.0", "error": { "code": code, "message": message }, "id": id })
    }

    #[tokio::test]
    async fn successful_call_returns_result_with_the_same_id() {
        let reply = run(r#"{"jsonrpc":"2.0","method":"echo","params":{"a":1},"id":7}"#).await;

        assert_eq!(
            to_json(reply),
            json!({ "jsonrpc": "2.0", "result": { "a": 1 }, "id": 7 })
        );
    }

    #[tokio::test]
    async fn string_and_null_ids_are_echoed() {
        let string_id = run(r#"{"jsonrpc":"2.0","method":"echo","id":"x"}"#).await;
        let null_id = run(r#"{"jsonrpc":"2.0","method":"echo","id":null}"#).await;

        assert_eq!(to_json(string_id)["id"], json!("x"));
        assert_eq!(
            to_json(null_id),
            json!({ "jsonrpc": "2.0", "result": null, "id": null })
        );
    }

    #[tokio::test]
    async fn ids_up_to_the_javascript_safe_limit_are_echoed() {
        for id in [9_007_199_254_740_991_i64, -9_007_199_254_740_991, 0] {
            let body = json!({ "jsonrpc": "2.0", "method": "echo", "id": id }).to_string();

            assert_eq!(to_json(run(&body).await)["id"], json!(id));
        }
    }

    #[tokio::test]
    async fn malformed_json_is_a_parse_error_with_null_id() {
        for body in [r#"{"jsonrpc":"2.0","method":"echo","id":1"#, "", "not json"] {
            assert_eq!(
                to_json(run(body).await),
                error(-32700, "Parse error", Value::Null)
            );
        }
    }

    #[tokio::test]
    async fn invalid_requests_echo_a_readable_id() {
        let cases = [
            r#"{"method":"echo","id":1}"#,
            r#"{"jsonrpc":"1.0","method":"echo","id":1}"#,
            r#"{"jsonrpc":2.0,"method":"echo","id":1}"#,
            r#"{"jsonrpc":"2.0","method":5,"id":1}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","method":"echo","params":"x","id":1}"#,
            r#"{"jsonrpc":"2.0","method":"echo","params":null,"id":1}"#,
        ];
        for body in cases {
            assert_eq!(
                to_json(run(body).await),
                error(-32600, "Invalid Request", json!(1)),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_requests_without_a_readable_id_get_null() {
        let cases = [
            "1",
            r#""echo""#,
            "null",
            r#"{"jsonrpc":"2.0","method":"echo","id":{}}"#,
            r#"{"jsonrpc":"2.0","method":"echo","id":[1]}"#,
            r#"{"jsonrpc":"2.0","method":"echo","id":true}"#,
            r#"{"jsonrpc":"2.0","method":"echo","id":1.5}"#,
            r#"{"jsonrpc":"2.0","method":"echo","id":18446744073709551615}"#,
            r#"{"jsonrpc":"2.0","method":"echo","id":9007199254740992}"#,
            r#"{"jsonrpc":"2.0","method":"echo","id":-9007199254740992}"#,
            // i64::MIN has no positive counterpart; `abs()` on it overflows.
            r#"{"jsonrpc":"2.0","method":"echo","id":-9223372036854775808}"#,
            r#"{"jsonrpc":"2.0","method":"echo","id":9223372036854775807}"#,
            r#"{"jsonrpc":"1.0","method":"echo"}"#,
            r#"{"foo":"boo"}"#,
        ];
        for body in cases {
            assert_eq!(
                to_json(run(body).await),
                error(-32600, "Invalid Request", Value::Null),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_requests_never_reach_the_handler() {
        let handler = EchoHandler::default();

        dispatch(&handler, br#"{"jsonrpc":"2.0","method":"echo","id":{}}"#).await;
        dispatch(&handler, br#"{"jsonrpc":"2.0","method":"echo","params":1}"#).await;

        assert!(handler.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let reply = run(r#"{"jsonrpc":"2.0","method":"nope","id":3}"#).await;

        assert_eq!(to_json(reply), error(-32601, "Method not found", json!(3)));
    }

    #[tokio::test]
    async fn handler_errors_keep_their_code_and_message() {
        let bad_params = run(r#"{"jsonrpc":"2.0","method":"needsObject","params":[],"id":1}"#);
        let callsheet = run(r#"{"jsonrpc":"2.0","method":"fail","id":2}"#);

        assert_eq!(
            to_json(bad_params.await),
            error(-32602, "Invalid params", json!(1))
        );
        assert_eq!(
            to_json(callsheet.await),
            error(1003, "Unknown run", json!(2))
        );
    }

    #[tokio::test]
    async fn a_notification_runs_and_gets_nothing() {
        let handler = EchoHandler::default();

        let reply = dispatch(&handler, br#"{"jsonrpc":"2.0","method":"echo"}"#).await;
        let failing = dispatch(&handler, br#"{"jsonrpc":"2.0","method":"nope"}"#).await;

        assert_eq!(reply, Reply::Nothing);
        assert_eq!(failing, Reply::Nothing);
        assert_eq!(*handler.calls.lock().unwrap(), ["echo", "nope"]);
    }

    #[tokio::test]
    async fn empty_batch_is_one_invalid_request() {
        assert_eq!(
            to_json(run("[]").await),
            error(-32600, "Invalid Request", Value::Null)
        );
    }

    #[tokio::test]
    async fn batch_over_the_limit_is_one_invalid_request_and_runs_nothing() {
        let handler = EchoHandler::default();
        let entry = json!({ "jsonrpc": "2.0", "method": "echo", "id": 1 });
        let batch = Value::Array(vec![entry; MAX_BATCH_LEN + 1]);

        let reply = dispatch(&handler, batch.to_string().as_bytes()).await;

        assert_eq!(
            to_json(reply),
            error(-32600, "Invalid Request", Value::Null)
        );
        assert!(handler.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn batch_at_the_limit_is_processed() {
        let batch: Vec<Value> = (0..MAX_BATCH_LEN)
            .map(|i| json!({ "jsonrpc": "2.0", "method": "echo", "params": [i], "id": i }))
            .collect();

        let reply = run(&Value::Array(batch).to_string()).await;

        let Reply::Batch(responses) = reply else {
            panic!("expected a batch")
        };
        assert_eq!(responses.len(), MAX_BATCH_LEN);
    }

    #[tokio::test]
    async fn batch_of_only_notifications_gets_nothing() {
        let handler = EchoHandler::default();
        let body = br#"[{"jsonrpc":"2.0","method":"echo"},{"jsonrpc":"2.0","method":"fail"}]"#;

        assert_eq!(dispatch(&handler, body).await, Reply::Nothing);
        assert_eq!(*handler.calls.lock().unwrap(), ["echo", "fail"]);
    }

    #[tokio::test]
    async fn mixed_batch_runs_in_order_and_answers_only_calls() {
        let handler = EchoHandler::default();
        let body = br#"[
            {"jsonrpc":"2.0","method":"echo","params":["a"],"id":1},
            {"jsonrpc":"2.0","method":"echo","params":["b"]},
            {"foo":"boo"},
            1,
            {"jsonrpc":"2.0","method":"nope","id":"x"},
            {"jsonrpc":"2.0","method":"fail","id":2}
        ]"#;

        let reply = dispatch(&handler, body).await;

        assert_eq!(
            to_json(reply),
            json!([
                { "jsonrpc": "2.0", "result": ["a"], "id": 1 },
                error(-32600, "Invalid Request", Value::Null),
                error(-32600, "Invalid Request", Value::Null),
                error(-32601, "Method not found", json!("x")),
                error(1003, "Unknown run", json!(2)),
            ])
        );
        assert_eq!(
            *handler.calls.lock().unwrap(),
            ["echo", "echo", "nope", "fail"]
        );
    }

    #[tokio::test]
    async fn nested_batches_are_invalid_entries() {
        let reply = run(r#"[[{"jsonrpc":"2.0","method":"echo","id":1}]]"#).await;

        assert_eq!(
            to_json(reply),
            json!([error(-32600, "Invalid Request", Value::Null)])
        );
    }

    #[tokio::test]
    async fn error_messages_never_echo_the_request() {
        let secret = "s3cret-value";
        let bodies = [
            format!(r#"{{"jsonrpc":"2.0","method":"{secret}","id":1}}"#),
            format!(r#"{{"jsonrpc":"{secret}","method":"echo","id":1}}"#),
            format!(r#"{{"jsonrpc":"2.0","method":"needsObject","params":["{secret}"],"id":1}}"#),
            format!(r#"{{"{secret}":"#),
        ];
        for body in bodies {
            let text = to_json(run(&body).await).to_string();
            assert!(!text.contains(secret), "{text}");
        }
    }
}
