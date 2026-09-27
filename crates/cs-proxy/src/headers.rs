//! Header rules (design, "Headers") and run grouping (design, "Run grouping and
//! what is recorded"). Owned by task 1 (`forward`).
//!
//! Foundation stub: copies headers unchanged and always uses the default run.

use http::HeaderMap;

/// Headers to send upstream: every request header except hop-by-hop ones,
/// `host`, `accept-encoding` and `x-callsheet-*`.
pub fn to_upstream(request: &HeaderMap) -> HeaderMap {
    request.clone()
}

/// Headers to send the client: every upstream response header except
/// hop-by-hop ones.
pub fn to_client(response: &HeaderMap) -> HeaderMap {
    response.clone()
}

/// The run a request belongs to: `x-callsheet-run`, else
/// `cc-<x-claude-code-session-id>`, else `default_run`; invalid values fall
/// through to the next rule.
pub fn run_id(_request: &HeaderMap, default_run: &str) -> String {
    default_run.to_owned()
}
