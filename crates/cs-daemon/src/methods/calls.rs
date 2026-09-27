//! `calls.list` (feature 03, requirement 7). Owned by unit `calls-list`.
//!
//! Params ([`CallsListParams`]) are by name only, and absent params mean the
//! defaults: `limit` 1 to [`MAX_LIMIT`] ([`DEFAULT_LIMIT`] when absent), else
//! -32602; `before` optional. The result ([`CallsListResult`]) holds the
//! `llm.call` events at a global position below `before`, newest first.
//!
//! One read ([`Store::read`]: the read-only connection, under the shared read
//! gate) takes `limit + 1` rows; the extra row only says there is an older
//! page. `nextBefore` is then the position of the oldest row this page took,
//! so the next page starts exactly below it: no overlap, no gap.
//!
//! A page is also cut short by size: entries go in, newest first, until the
//! next one would take their serialized total over [`MAX_PAGE_BYTES`].
//! `nextBefore` is then the position of the last row taken, and the next page
//! starts with the entry that didn't fit. The first entry always goes in, so
//! paging never sticks, even on one entry over the budget.
//!
//! A row whose `run_id` or `body` isn't text (a BLOB, or invalid UTF-8), or
//! whose body doesn't parse as an [`LlmCallRecord`], is skipped and logged by
//! its global position only. The list is a view: an event is never changed,
//! and a record another daemon version wrote (a field added as required, or one
//! removed) must not make every page fail. `events.verify` still covers the
//! skipped event. A skipped row still counts towards `limit` and `nextBefore`.
//!
//! So a page can hold fewer than `limit` calls (even none, after skipped rows)
//! and still have a `nextBefore`. A client pages on until `nextBefore` is
//! absent; a short or empty page is not the end.
//!
//! The query filters on `kind` without an index (schema version 1 has none on
//! `events.kind`): it walks `events` down from `before` by primary key until it
//! has `limit + 1` calls. That costs one row per event scanned, so it is slow
//! only when calls are rare among many newer events.
//!
//! Store failures are -32603 with a fixed message; the cause goes to the log.

use cs_core::llm::{CallEntry, CallsListParams, CallsListResult, LLM_CALL_KIND, LlmCallRecord};
use cs_store::{Store, StoreError};
use serde_json::Value;

use super::core::object_params;
use crate::rpc::RpcError;

/// `limit` when the params don't give one.
pub const DEFAULT_LIMIT: u32 = 50;

/// Largest `limit` accepted.
pub const MAX_LIMIT: u32 = 200;

/// Most bytes of serialized entries one page holds, past its first entry.
///
/// The desktop refuses a reply over 64 KiB (`MAX_RESPONSE_BYTES` in
/// `apps/desktop/src/main/daemon.ts`). Each record carries every recorded
/// rate-limit header, so 50 records can already be more than that. 48 KiB of
/// entries leaves 16 KiB for the JSON-RPC envelope, the commas between entries
/// and `nextBefore`.
///
/// One entry over the budget still goes out, alone. If it is also over 64 KiB
/// (the store takes a body up to 256 KiB, and nothing caps the recorded
/// headers yet), the desktop refuses that reply, so its list stops at that
/// call. Capping the recorded headers where they are recorded (`observe`)
/// keeps every record well under the budget.
pub const MAX_PAGE_BYTES: usize = 48 * 1024;

/// Newest first, below `?2`, one more row than the page holds.
const PAGE_SQL: &str = "SELECT global_pos, run_id, body FROM events \
     WHERE kind = ?1 AND global_pos < ?2 ORDER BY global_pos DESC LIMIT ?3";

/// `calls.list`: one page of recorded calls, newest first.
pub async fn list(store: &Store, params: Option<Value>) -> Result<Value, RpcError> {
    let params: CallsListParams = match params {
        None => CallsListParams::default(),
        some => object_params(some)?,
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(RpcError::invalid_params());
    }
    // Positions are SQLite integers, at most `i64::MAX`: a larger `before` (or
    // none) means below every position.
    let before = params
        .before
        .map_or(i64::MAX, |before| i64::try_from(before).unwrap_or(i64::MAX));
    let fetch = i64::from(limit) + 1;
    let rows = store
        .read(move |conn| {
            let mut statement = conn.prepare(PAGE_SQL)?;
            let rows = statement.query_map((LLM_CALL_KIND, before, fetch), |row| {
                // A BLOB or invalid UTF-8 reads as `None` instead of failing
                // the page; `into_entry` skips the row.
                Ok(StoredCall {
                    global_pos: row.get(0)?,
                    run_id: row.get_ref(1)?.as_str().ok().map(str::to_owned),
                    body: row.get_ref(2)?.as_str().ok().map(str::to_owned),
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|error| store_failure(&error))?;
    serde_json::to_value(page(rows, limit)).map_err(|_| RpcError::internal_error())
}

/// An `llm.call` row as read, before its body is parsed. `run_id` and `body`
/// are `None` when the column isn't valid text.
#[derive(Debug)]
struct StoredCall {
    global_pos: i64,
    run_id: Option<String>,
    body: Option<String>,
}

impl StoredCall {
    /// The entry, or `None` (logged by position only) if the row isn't a
    /// record this build can read.
    fn into_entry(self) -> Option<CallEntry> {
        let parsed = self
            .body
            .and_then(|body| serde_json::from_str::<LlmCallRecord>(&body).ok());
        match (u64::try_from(self.global_pos), self.run_id, parsed) {
            (Ok(global_pos), Some(run_id), Some(call)) => Some(CallEntry {
                global_pos,
                run_id,
                call,
            }),
            // The parse error can quote the body, so it isn't logged.
            _ => {
                tracing::warn!(
                    global_pos = self.global_pos,
                    "calls.list skipped an llm.call event it cannot read"
                );
                None
            }
        }
    }
}

/// Builds the result from up to `limit + 1` rows, newest first, within
/// [`MAX_PAGE_BYTES`].
fn page(mut rows: Vec<StoredCall>, limit: u32) -> CallsListResult {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = rows.len() > limit;
    rows.truncate(limit);
    let mut calls = Vec::new();
    let mut bytes: usize = 0;
    // The position of the last row taken, whether read or skipped.
    let mut last_taken = None;
    for row in rows {
        let global_pos = row.global_pos;
        if let Some(entry) = row.into_entry() {
            let size = serialized_len(&entry);
            if !calls.is_empty() && bytes.saturating_add(size) > MAX_PAGE_BYTES {
                return CallsListResult {
                    calls,
                    next_before: last_taken,
                };
            }
            bytes = bytes.saturating_add(size);
            calls.push(entry);
        }
        last_taken = u64::try_from(global_pos).ok();
    }
    CallsListResult {
        calls,
        next_before: last_taken.filter(|_| more),
    }
}

/// Bytes of `entry` in the reply, which is compact JSON.
fn serialized_len(entry: &CallEntry) -> usize {
    serde_json::to_vec(entry).map_or(usize::MAX, |json| json.len())
}

/// Logs the cause and gives the client a fixed message only. Store errors
/// name no content, token or request.
fn store_failure(error: &StoreError) -> RpcError {
    tracing::error!(%error, "calls.list could not read the event log");
    RpcError::internal_error()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use cs_core::llm::{CallOutcome, Usage};
    use cs_core::rpc::codes;
    use cs_store::AppendEvent;
    use cs_store::secrets::InMemorySecretStore;
    use serde_json::json;

    use super::*;

    fn open(dir: &std::path::Path) -> Store {
        Store::open(dir, Arc::new(InMemorySecretStore::new([4; 32]))).unwrap()
    }

    /// A record told apart by `n` (its `requestBytes` and `startedAtMs`).
    fn record(n: u64) -> LlmCallRecord {
        LlmCallRecord {
            provider: "anthropic".into(),
            method: "POST".into(),
            path: "/v1/messages".into(),
            status: 200,
            outcome: CallOutcome::Completed,
            streamed: true,
            model: Some("claude-opus-5".into()),
            request_id: Some(format!("req_{n}")),
            stop_reason: Some("end_turn".into()),
            error_type: None,
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 20,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(5),
            }),
            started_at_ms: 1_790_000_000_000 + n,
            ttfb_ms: Some(120),
            duration_ms: 900,
            request_bytes: n,
            response_bytes: 2048,
            rate_limit_headers: BTreeMap::from([(
                "anthropic-ratelimit-requests-remaining".to_owned(),
                "49".to_owned(),
            )]),
            trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
            user_agent: Some("claude-cli/2.1.0".into()),
            content_truncated: None,
        }
    }

    /// Appends one event and returns its global position.
    async fn append(store: &Store, run: &str, kind: &str, body: Value) -> u64 {
        append_with(store, run, kind, body, Vec::new()).await
    }

    async fn append_with(
        store: &Store,
        run: &str,
        kind: &str,
        body: Value,
        content: Vec<Vec<u8>>,
    ) -> u64 {
        store
            .append(AppendEvent {
                run_id: run.to_owned(),
                kind: kind.to_owned(),
                ts_ms: 1_790_000_000_000,
                body,
                content,
            })
            .await
            .unwrap()
            .global_pos
    }

    async fn append_call(store: &Store, run: &str, n: u64) -> u64 {
        let body = serde_json::to_value(record(n)).unwrap();
        append(store, run, LLM_CALL_KIND, body).await
    }

    async fn list_ok(store: &Store, params: Option<Value>) -> CallsListResult {
        let value = list(store, params).await.unwrap();
        serde_json::from_value(value).unwrap()
    }

    fn positions(result: &CallsListResult) -> Vec<u64> {
        result.calls.iter().map(|entry| entry.global_pos).collect()
    }

    #[tokio::test]
    async fn an_empty_store_lists_no_calls_and_no_next_page() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());

        for params in [None, Some(json!({})), Some(json!({ "limit": 200 }))] {
            let value = list(&store, params.clone()).await.unwrap();
            assert_eq!(value, json!({ "calls": [] }), "{params:?}");
        }
    }

    #[tokio::test]
    async fn only_llm_call_events_are_listed_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        append(&store, "run-a", "run.started", json!({})).await;
        let first = append_call(&store, "run-a", 1).await;
        append(&store, "run-a", "llm.calls", json!({ "n": 1 })).await;
        append(&store, "run-b", "llm.call.v2", json!({ "n": 2 })).await;
        let second = append_call(&store, "run-b", 2).await;
        append(&store, "run-b", "test.event", json!({})).await;

        let value = list(&store, None).await.unwrap();

        assert_eq!(
            value["calls"][0],
            json!({
                "globalPos": second,
                "runId": "run-b",
                "call": serde_json::to_value(record(2)).unwrap(),
            })
        );
        assert!(value.get("nextBefore").is_none());
        let result: CallsListResult = serde_json::from_value(value).unwrap();
        assert_eq!(
            result.calls,
            vec![
                CallEntry {
                    global_pos: second,
                    run_id: "run-b".into(),
                    call: record(2),
                },
                CallEntry {
                    global_pos: first,
                    run_id: "run-a".into(),
                    call: record(1),
                },
            ]
        );
    }

    #[tokio::test]
    async fn pages_follow_next_before_without_overlap_or_gap() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let mut calls = Vec::new();
        for n in 1..=5 {
            // Other events in between, so positions of calls aren't adjacent.
            append(&store, "run", "test.event", json!({})).await;
            calls.push(append_call(&store, "run", n).await);
        }
        calls.reverse();

        let one = list_ok(&store, Some(json!({ "limit": 2 }))).await;
        assert_eq!(positions(&one), calls[0..2]);
        assert_eq!(one.next_before, Some(calls[1]));

        let two = list_ok(&store, Some(json!({ "limit": 2, "before": calls[1] }))).await;
        assert_eq!(positions(&two), calls[2..4]);
        assert_eq!(two.next_before, Some(calls[3]));

        let three = list_ok(&store, Some(json!({ "limit": 2, "before": calls[3] }))).await;
        assert_eq!(positions(&three), calls[4..5]);
        assert_eq!(three.next_before, None, "the last page");

        let requests: Vec<u64> = [one, two, three]
            .iter()
            .flat_map(|page| page.calls.iter().map(|entry| entry.call.request_bytes))
            .collect();
        assert_eq!(requests, [5, 4, 3, 2, 1]);
    }

    #[tokio::test]
    async fn a_full_last_page_has_no_next_before() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        for n in 1..=4 {
            append_call(&store, "run", n).await;
        }

        let one = list_ok(&store, Some(json!({ "limit": 2 }))).await;
        assert_eq!(one.next_before, Some(3));
        let two = list_ok(&store, Some(json!({ "limit": 2, "before": 3 }))).await;
        assert_eq!(positions(&two), [2, 1]);
        assert_eq!(two.next_before, None);
    }

    #[tokio::test]
    async fn before_bounds_the_page() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        for n in 1..=3 {
            append_call(&store, "run", n).await;
        }

        for before in [4, 1_000, 1 << 53, u64::MAX] {
            let result = list_ok(&store, Some(json!({ "before": before }))).await;
            assert_eq!(positions(&result), [3, 2, 1], "before {before}");
            assert_eq!(result.next_before, None);
        }
        let result = list_ok(&store, Some(json!({ "before": 3 }))).await;
        assert_eq!(positions(&result), [2, 1], "`before` itself is excluded");
        for before in [1, 0] {
            let value = list(&store, Some(json!({ "before": before })))
                .await
                .unwrap();
            assert_eq!(value, json!({ "calls": [] }), "before {before}");
        }
    }

    #[tokio::test]
    async fn the_default_limit_is_50() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        for n in 1..=51 {
            append_call(&store, "run", n).await;
        }

        let result = list_ok(&store, None).await;

        assert_eq!(result.calls.len(), 50);
        assert_eq!(result.calls[0].global_pos, 51);
        assert_eq!(result.next_before, Some(2));
        let rest = list_ok(&store, Some(json!({ "before": 2 }))).await;
        assert_eq!(positions(&rest), [1]);
    }

    #[tokio::test]
    async fn invalid_params_are_32602() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        append_call(&store, "run", 1).await;

        for params in [
            json!({ "limit": 0 }),
            json!({ "limit": 201 }),
            json!({ "limit": -1 }),
            json!({ "limit": "x" }),
            json!({ "limit": 2.5 }),
            json!({ "limit": null, "extra": 1 }),
            json!({ "before": -1 }),
            json!({ "before": "1" }),
            json!({ "cursor": 1 }),
            json!([]),
            json!([10]),
            json!([10, 1]),
        ] {
            let error = list(&store, Some(params.clone())).await.unwrap_err();
            assert_eq!(error, RpcError::invalid_params(), "{params}");
            assert_eq!(error.code, codes::INVALID_PARAMS);
        }
        for limit in [1, 200] {
            let result = list_ok(&store, Some(json!({ "limit": limit }))).await;
            assert_eq!(positions(&result), [1], "limit {limit}");
        }
    }

    #[tokio::test]
    async fn a_body_that_does_not_parse_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let old = append_call(&store, "run", 1).await;
        let future = append(&store, "run", LLM_CALL_KIND, json!({ "version": 2 })).await;
        let wrong_type = serde_json::to_value(record(2))
            .map(|mut body| {
                body["status"] = json!("200");
                body
            })
            .unwrap();
        let broken = append(&store, "run", LLM_CALL_KIND, wrong_type).await;
        let new = append_call(&store, "run", 3).await;

        let all = list_ok(&store, None).await;
        assert_eq!(positions(&all), [new, old]);

        // A skipped row still ends its page, so the next page starts below it.
        let one = list_ok(&store, Some(json!({ "limit": 2 }))).await;
        assert_eq!(positions(&one), [new]);
        assert_eq!(one.next_before, Some(broken));
        let two = list_ok(&store, Some(json!({ "limit": 1, "before": broken }))).await;
        assert!(two.calls.is_empty());
        assert_eq!(two.next_before, Some(future), "paging goes on");
        let three = list_ok(&store, Some(json!({ "limit": 1, "before": future }))).await;
        assert_eq!(positions(&three), [old]);
        assert_eq!(three.next_before, None);
    }

    /// `record(n)` with one rate-limit header of `bytes` bytes.
    fn record_of_size(n: u64, bytes: usize) -> LlmCallRecord {
        let mut call = record(n);
        call.rate_limit_headers
            .insert("anthropic-ratelimit-padding".into(), "x".repeat(bytes));
        call
    }

    #[tokio::test]
    async fn a_page_stops_at_the_byte_budget_but_always_takes_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let oldest = append_call(&store, "run", 1).await;
        let huge = serde_json::to_value(record_of_size(2, MAX_PAGE_BYTES + 1024)).unwrap();
        let huge = append(&store, "run", LLM_CALL_KIND, huge).await;
        let skipped = append(&store, "run", LLM_CALL_KIND, json!({ "version": 2 })).await;
        let newest = append_call(&store, "run", 3).await;

        // The huge entry doesn't fit after `newest`; the skipped row before it
        // was taken, so the next page starts below that row.
        let one = list_ok(&store, None).await;
        assert_eq!(positions(&one), [newest]);
        assert_eq!(one.next_before, Some(skipped));

        // Alone on its page, over the budget, then the rest.
        let two = list_ok(&store, Some(json!({ "before": skipped }))).await;
        assert_eq!(positions(&two), [huge]);
        assert!(serialized_len(&two.calls[0]) > MAX_PAGE_BYTES);
        assert_eq!(two.next_before, Some(huge));
        let three = list_ok(&store, Some(json!({ "before": huge }))).await;
        assert_eq!(positions(&three), [oldest]);
        assert_eq!(three.next_before, None);
    }

    #[tokio::test]
    async fn entries_fill_a_page_up_to_the_budget_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let one_entry = |n: u64, pos: u64, bytes: usize| {
            serialized_len(&CallEntry {
                global_pos: pos,
                run_id: "run".into(),
                call: record_of_size(n, bytes),
            })
        };
        // Entries of exactly half the budget: every `n` and position below has
        // one digit, so they serialize to the same length.
        let padding = MAX_PAGE_BYTES / 2 - one_entry(1, 1, 0);
        assert_eq!(one_entry(1, 1, padding), MAX_PAGE_BYTES / 2);
        append_call(&store, "run", 1).await;
        for (n, bytes) in [(2, padding), (3, padding), (4, padding + 1)] {
            let body = serde_json::to_value(record_of_size(n, bytes)).unwrap();
            append(&store, "run", LLM_CALL_KIND, body).await;
        }

        let one = list_ok(&store, None).await;
        assert_eq!(positions(&one), [4], "one byte over the budget");
        assert_eq!(one.next_before, Some(4));
        let two = list_ok(&store, Some(json!({ "before": 4 }))).await;
        assert_eq!(positions(&two), [3, 2], "exactly the budget");
        assert_eq!(two.next_before, Some(2));
        let three = list_ok(&store, Some(json!({ "before": 2 }))).await;
        assert_eq!(positions(&three), [1]);
        assert_eq!(three.next_before, None);
    }

    #[tokio::test]
    async fn a_row_that_is_not_text_is_skipped_not_a_failed_page() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let first = append_call(&store, "run", 1).await;
        store.close().await.unwrap();

        // The store only writes text; these rows go in with plain SQL, as a
        // damaged or foreign database could hold them (no hash chain;
        // `calls.list` doesn't read it).
        let body = serde_json::to_string(&record(2)).unwrap();
        let conn = cs_store::db::open_writer(dir.path()).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO runs VALUES (x'ff', 0, 1, '');
             INSERT INTO events VALUES (2, 'run', 2, 'llm.call', 0, CAST('{body}' AS BLOB), '', '');
             INSERT INTO events VALUES (3, 'run', 3, 'llm.call', 0, CAST(x'7bff7d' AS TEXT), '', '');
             INSERT INTO events VALUES (4, x'ff', 1, 'llm.call', 0, '{body}', '', '');"
        ))
        .unwrap();
        drop(conn);
        let store = open(dir.path());
        let last = append_call(&store, "other", 3).await;

        const TYPES_SQL: &str = "SELECT typeof(body) || '/' || typeof(run_id) FROM events \
             WHERE global_pos BETWEEN 2 AND 4 ORDER BY global_pos";
        let kinds: Vec<String> = store
            .read(|conn| {
                let mut statement = conn.prepare(TYPES_SQL)?;
                let rows = statement.query_map((), |row| row.get(0))?;
                rows.collect()
            })
            .await
            .unwrap();
        assert_eq!(kinds, ["blob/text", "text/text", "text/blob"]);

        let all = list_ok(&store, None).await;
        assert_eq!(positions(&all), [last, first]);
        let page = list_ok(&store, Some(json!({ "limit": 2 }))).await;
        assert_eq!(positions(&page), [last]);
        assert_eq!(page.next_before, Some(4));
    }

    #[test]
    fn a_row_with_a_negative_position_is_skipped() {
        let stored = StoredCall {
            global_pos: -1,
            run_id: Some("run".into()),
            body: Some(serde_json::to_string(&record(1)).unwrap()),
        };
        assert!(stored.into_entry().is_none());
    }

    #[tokio::test]
    async fn a_call_with_captured_content_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        store.set_capture_content(true).await.unwrap();
        let body = serde_json::to_value(record(7)).unwrap();
        let pos = append_with(
            &store,
            "run",
            LLM_CALL_KIND,
            body,
            vec![b"request".to_vec(), b"response".to_vec()],
        )
        .await;

        // The store adds `content` (the addresses) to the stored body.
        let result = list_ok(&store, None).await;

        assert_eq!(
            result.calls,
            [CallEntry {
                global_pos: pos,
                run_id: "run".into(),
                call: record(7),
            }]
        );
    }

    #[test]
    fn store_failures_are_a_fixed_internal_error() {
        let error = store_failure(&StoreError::TaskFailed);
        assert_eq!(error, RpcError::internal_error());
        assert_eq!(error.message, "Internal error");
    }

    /// A measurement, not a check: `cargo test --release -p cs-daemon --lib
    /// calls::tests::page_time_on_100k_events -- --ignored --nocapture`.
    /// Rows go in with plain SQL (no hash chain; `calls.list` doesn't read it).
    #[tokio::test]
    #[ignore = "timing measurement; run by hand in release"]
    async fn page_time_on_100k_events() {
        const EVENTS: u32 = 100_000;
        let body = serde_json::to_string(&record(1))
            .unwrap()
            .replace('\'', "''");
        for (layout, kind_expr) in [
            (
                "interleaved: every other event a call",
                "CASE x % 2 WHEN 0 THEN 'llm.call' ELSE 'test.event' END",
            ),
            (
                "50 000 calls, then 50 000 other events",
                "CASE WHEN x <= 50000 THEN 'llm.call' ELSE 'test.event' END",
            ),
            ("no calls at all", "'test.event'"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            open(dir.path()).close().await.unwrap();
            let conn = cs_store::db::open_writer(dir.path()).unwrap();
            conn.execute_batch(&format!(
                "BEGIN;
                 INSERT INTO runs VALUES ('bench', 0, {EVENTS}, '');
                 WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < {EVENTS})
                 INSERT INTO events SELECT x, 'bench', x, {kind_expr}, 0, '{body}', '', '' FROM n;
                 COMMIT;"
            ))
            .unwrap();
            drop(conn);
            let store = open(dir.path());
            assert_eq!(
                store.last_global_position().await.unwrap(),
                u64::from(EVENTS)
            );

            for (label, params) in [
                ("newest page, limit 50", json!({})),
                ("newest page, limit 200", json!({ "limit": 200 })),
                (
                    "page below 1000, limit 200",
                    json!({ "limit": 200, "before": 1000 }),
                ),
            ] {
                let runs = 20;
                let started = std::time::Instant::now();
                for _ in 0..runs {
                    list(&store, Some(params.clone())).await.unwrap();
                }
                let each = started.elapsed() / runs;
                println!("{layout}; {label}: {each:?} per call");
            }
        }
    }
}
