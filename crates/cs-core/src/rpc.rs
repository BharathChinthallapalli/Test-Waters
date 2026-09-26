//! JSON-RPC 2.0 envelope of the control API (ADR 0003, feature 02 design).
//!
//! The daemon parses requests from raw JSON so it can answer malformed ones with the
//! right error; these types are what well-formed traffic looks like on the wire, and
//! what TypeScript clients use.

use serde::{Deserialize, Serialize};

/// The only accepted value of the `jsonrpc` member.
pub const JSONRPC_VERSION: &str = "2.0";

/// The largest batch the daemon processes; a bigger one is `-32600`.
pub const MAX_BATCH_LEN: usize = 16;

/// A request `id`. Numbers stay below 2^53, so they are exact in JavaScript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(untagged)]
pub enum RequestId {
    Number(#[cfg_attr(test, ts(type = "number"))] i64),
    String(String),
}

/// A call with an `id`. A notification is the same object without `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct Request<P> {
    pub jsonrpc: String,
    pub method: String,
    pub params: P,
    pub id: RequestId,
}

/// A JSON-RPC error. `data` is never sent: messages carry everything a client needs
/// and must not echo request contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
}

/// A response: exactly one of `result` or `error`. `id` is `null` only when the
/// request's `id` could not be read (parse error or invalid request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct Response<R> {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub result: Option<R>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub error: Option<ErrorObject>,
    pub id: Option<RequestId>,
}

/// Error codes. The negative ones are JSON-RPC 2.0's; Callsheet's own codes sit
/// outside the reserved range (ADR 0003).
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    /// The OS keychain can't hold the content key, so capture stays off.
    pub const KEYCHAIN_UNAVAILABLE: i32 = 1001;
    /// `content.erase` was given a plan ID that no longer matches the run.
    pub const ERASE_PLAN_OUT_OF_DATE: i32 = 1002;
    pub const UNKNOWN_RUN: i32 = 1003;
    /// Content was deleted but a copy may remain until the retry succeeds.
    pub const ERASURE_PENDING: i32 = 1004;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_response_has_result_and_no_error() {
        let response = Response {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            result: Some(json!({ "daemonVersion": "0.1.0" })),
            error: None,
            id: Some(RequestId::Number(1)),
        };

        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({ "jsonrpc": "2.0", "result": { "daemonVersion": "0.1.0" }, "id": 1 })
        );
    }

    #[test]
    fn error_response_without_readable_id_has_null_id() {
        let response: Response<()> = Response {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            result: None,
            error: Some(ErrorObject {
                code: codes::PARSE_ERROR,
                message: "Parse error".to_owned(),
            }),
            id: None,
        };

        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({ "jsonrpc": "2.0", "error": { "code": -32700, "message": "Parse error" }, "id": null })
        );
    }

    #[test]
    fn request_ids_are_numbers_or_strings() {
        assert_eq!(
            serde_json::from_value::<RequestId>(json!(7)).unwrap(),
            RequestId::Number(7)
        );
        assert_eq!(
            serde_json::from_value::<RequestId>(json!("a")).unwrap(),
            RequestId::String("a".to_owned())
        );
    }
}
