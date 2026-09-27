//! Header rules (design, "Headers") and run grouping (design, "Run grouping and
//! what is recorded"). Owned by task 1 (`forward`).
//!
//! Hop-by-hop headers are the design's list (`connection`, `keep-alive`,
//! `proxy-connection`, `proxy-authorization`, `te`, `trailer`,
//! `transfer-encoding`, `upgrade`) plus every name listed in `connection`.
//! Everything else passes in its original order, multiple values included:
//! `anthropic-*`, `authorization` and `x-api-key` are forwarded as they came.

use std::collections::HashSet;

use http::HeaderMap;
use http::header::{ACCEPT_ENCODING, CONNECTION, HOST, HeaderName};

/// Callsheet's own request headers start with this and are never forwarded.
pub const CALLSHEET_PREFIX: &str = "x-callsheet-";

/// Names the run a call belongs to (settable with Claude Code's
/// `ANTHROPIC_CUSTOM_HEADERS`).
pub const RUN_HEADER: &str = "x-callsheet-run";

/// Claude Code's per-session header [GWP]; forwarded unchanged.
pub const SESSION_HEADER: &str = "x-claude-code-session-id";

/// Longest accepted run id, in bytes.
pub const MAX_RUN_ID_BYTES: usize = 256;

const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Headers to send upstream: every request header except hop-by-hop ones,
/// `host`, `accept-encoding` and `x-callsheet-*`.
///
/// `accept-encoding` goes so the upstream answers uncompressed and the observer
/// can read usage (a documented deviation from pure passthrough).
pub fn to_upstream(request: &HeaderMap) -> HeaderMap {
    let named = connection_named(request);
    filtered(request, |name| {
        !is_hop_by_hop(name)
            && !named.contains(name)
            && name != HOST
            && name != ACCEPT_ENCODING
            && !name.as_str().starts_with(CALLSHEET_PREFIX)
    })
}

/// Headers to send the client: every upstream response header except
/// hop-by-hop ones.
pub fn to_client(response: &HeaderMap) -> HeaderMap {
    let named = connection_named(response);
    filtered(response, |name| {
        !is_hop_by_hop(name) && !named.contains(name)
    })
}

/// The run a request belongs to: `x-callsheet-run`, else
/// `cc-<x-claude-code-session-id>`, else `default_run`; invalid values fall
/// through to the next rule.
pub fn run_id(request: &HeaderMap, default_run: &str) -> String {
    if let Some(run) = header_text(request, RUN_HEADER).filter(|run| valid_run_id(run)) {
        return run.to_owned();
    }
    if let Some(session) = header_text(request, SESSION_HEADER).filter(|id| !id.is_empty()) {
        let run = format!("cc-{session}");
        if valid_run_id(&run) {
            return run;
        }
    }
    default_run.to_owned()
}

/// A run id is 1 to [`MAX_RUN_ID_BYTES`] bytes of UTF-8 without control
/// characters.
pub fn valid_run_id(run: &str) -> bool {
    !run.is_empty() && run.len() <= MAX_RUN_ID_BYTES && !run.chars().any(char::is_control)
}

fn is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(&name.as_str())
}

/// The header names listed in `connection` (RFC 9110, section 7.6.1).
fn connection_named(headers: &HeaderMap) -> HashSet<HeaderName> {
    headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect()
}

/// Copies the headers `keep` accepts, preserving order and repeated values.
fn filtered(headers: &HeaderMap, keep: impl Fn(&HeaderName) -> bool) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if keep(name) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// The first value of `name` as UTF-8.
fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    std::str::from_utf8(headers.get(name)?.as_bytes()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    fn pairs(headers: &HeaderMap) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect()
    }

    #[test]
    fn to_upstream_drops_hop_by_hop_host_encoding_and_callsheet_headers() {
        let request = map(&[
            ("host", "127.0.0.1:4000"),
            ("connection", "keep-alive, X-Hop"),
            ("keep-alive", "timeout=5"),
            ("proxy-connection", "keep-alive"),
            ("proxy-authorization", "Basic c2VjcmV0"),
            ("te", "trailers"),
            ("trailer", "x-t"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "websocket"),
            ("x-hop", "1"),
            ("accept-encoding", "gzip, br"),
            ("x-callsheet-run", "run-1"),
            ("x-callsheet-other", "x"),
            ("anthropic-version", "2023-06-01"),
            ("anthropic-beta", "a,b,oauth-2025-04-20"),
            ("anthropic-beta", "second-line"),
            ("x-api-key", "sk-test"),
            ("authorization", "Bearer tok"),
            ("x-claude-code-session-id", "s1"),
            (
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
            ("content-type", "application/json"),
            ("content-length", "2"),
        ]);
        assert_eq!(
            pairs(&to_upstream(&request)),
            vec![
                ("anthropic-version".into(), "2023-06-01".into()),
                ("anthropic-beta".into(), "a,b,oauth-2025-04-20".into()),
                ("anthropic-beta".into(), "second-line".into()),
                ("x-api-key".into(), "sk-test".into()),
                ("authorization".into(), "Bearer tok".into()),
                ("x-claude-code-session-id".into(), "s1".into()),
                (
                    "traceparent".into(),
                    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into()
                ),
                ("content-type".into(), "application/json".into()),
                ("content-length".into(), "2".into()),
            ]
        );
    }

    #[test]
    fn to_client_drops_only_hop_by_hop_headers() {
        let response = map(&[
            ("connection", "x-hop"),
            ("x-hop", "1"),
            ("transfer-encoding", "chunked"),
            ("keep-alive", "timeout=5"),
            ("content-type", "text/event-stream"),
            ("request-id", "req_1"),
            ("anthropic-ratelimit-requests-remaining", "10"),
            ("set-cookie", "a=1"),
            ("set-cookie", "b=2"),
            ("content-encoding", "gzip"),
        ]);
        assert_eq!(
            pairs(&to_client(&response)),
            vec![
                ("content-type".into(), "text/event-stream".into()),
                ("request-id".into(), "req_1".into()),
                ("anthropic-ratelimit-requests-remaining".into(), "10".into()),
                ("set-cookie".into(), "a=1".into()),
                ("set-cookie".into(), "b=2".into()),
                ("content-encoding".into(), "gzip".into()),
            ]
        );
    }

    #[test]
    fn run_header_wins_then_session_then_default() {
        let both = map(&[
            ("x-callsheet-run", "mine"),
            ("x-claude-code-session-id", "s1"),
        ]);
        assert_eq!(run_id(&both, "proxy-1"), "mine");
        let session = map(&[("x-claude-code-session-id", "s1")]);
        assert_eq!(run_id(&session, "proxy-1"), "cc-s1");
        assert_eq!(run_id(&HeaderMap::new(), "proxy-1"), "proxy-1");
    }

    #[test]
    fn invalid_run_values_fall_through() {
        let long = "r".repeat(MAX_RUN_ID_BYTES + 1);
        let exact = "r".repeat(MAX_RUN_ID_BYTES);
        let too_long = map(&[
            ("x-callsheet-run", &long),
            ("x-claude-code-session-id", "s1"),
        ]);
        assert_eq!(run_id(&too_long, "d"), "cc-s1");
        assert_eq!(run_id(&map(&[("x-callsheet-run", &exact)]), "d"), exact);

        let mut control = HeaderMap::new();
        control.insert(RUN_HEADER, HeaderValue::from_bytes(b"run\tone").unwrap());
        control.insert(SESSION_HEADER, HeaderValue::from_static("s2"));
        assert_eq!(run_id(&control, "d"), "cc-s2");

        let empty = map(&[("x-callsheet-run", ""), ("x-claude-code-session-id", "")]);
        assert_eq!(run_id(&empty, "d"), "d");

        // `cc-` plus a 254-byte session id is 257 bytes: over the limit.
        let long_session = "s".repeat(MAX_RUN_ID_BYTES - 2);
        assert_eq!(
            run_id(&map(&[("x-claude-code-session-id", &long_session)]), "d"),
            "d"
        );

        let mut not_utf8 = HeaderMap::new();
        not_utf8.insert(RUN_HEADER, HeaderValue::from_bytes(b"\xff\xfe").unwrap());
        assert_eq!(run_id(&not_utf8, "d"), "d");
    }

    #[test]
    fn a_utf8_run_id_is_accepted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            RUN_HEADER,
            HeaderValue::from_bytes("läuft".as_bytes()).unwrap(),
        );
        assert_eq!(run_id(&headers, "d"), "läuft");
    }
}
