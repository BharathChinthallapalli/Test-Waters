//! `events.verify`. Owned by unit `verify`.
//!
//! Takes no params: `params` absent or `{}`, anything else is -32602. Runs
//! [`cs_store::verify::verify`] and returns its `VerifyResult`. A log that
//! fails verification is a successful call with `ok: false`; only a store that
//! can't be read is an error, -32603 with a fixed message, while the cause
//! goes to the log.
//!
//! Every chunk of the scan is a read on the store's blocking pool, so a call
//! dropped by the request timeout stops between chunks and leaves nothing
//! half-done.

use cs_store::{Store, StoreError};
use serde_json::Value;

use crate::rpc::RpcError;

/// Handles one `events.verify` call.
pub async fn handle(store: &Store, params: Option<Value>) -> Result<Value, RpcError> {
    match params {
        None => {}
        Some(Value::Object(members)) if members.is_empty() => {}
        Some(_) => return Err(RpcError::invalid_params()),
    }
    let result = cs_store::verify::verify(store)
        .await
        .map_err(internal_error)?;
    serde_json::to_value(result).map_err(|_| RpcError::internal_error())
}

/// Logs the cause and gives the client a fixed message only.
fn internal_error(error: StoreError) -> RpcError {
    tracing::error!(%error, "events.verify could not read the event log");
    RpcError::internal_error()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cs_core::control::{VerifyResult, methods};
    use cs_core::rpc::codes;
    use cs_store::AppendEvent;
    use cs_store::secrets::InMemorySecretStore;
    use serde_json::json;

    use super::*;

    fn open(dir: &std::path::Path) -> Store {
        Store::open(dir, Arc::new(InMemorySecretStore::new([7; 32]))).unwrap()
    }

    async fn with_events(dir: &std::path::Path) -> Store {
        let store = open(dir);
        for n in 0..3 {
            store
                .append(AppendEvent {
                    run_id: format!("run-{}", n % 2),
                    kind: "test.event".to_owned(),
                    ts_ms: 1_790_000_000_000 + n,
                    body: json!({ "n": n }),
                    content: Vec::new(),
                })
                .await
                .unwrap();
        }
        store
    }

    #[test]
    fn the_method_name_is_the_one_on_the_wire() {
        assert_eq!(methods::EVENTS_VERIFY, "events.verify");
    }

    #[tokio::test]
    async fn returns_the_verify_result_without_params_or_with_an_empty_object() {
        let dir = tempfile::tempdir().unwrap();
        let store = with_events(dir.path()).await;
        let expected = json!({ "ok": true, "eventsChecked": 3, "erasedEvents": 0 });

        for params in [None, Some(json!({}))] {
            let value = handle(&store, params).await.unwrap();
            assert_eq!(value, expected);
            let typed: VerifyResult = serde_json::from_value(value).unwrap();
            assert!(typed.ok);
        }
    }

    #[tokio::test]
    async fn an_empty_log_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        assert_eq!(
            handle(&store, None).await.unwrap(),
            json!({ "ok": true, "eventsChecked": 0, "erasedEvents": 0 })
        );
    }

    #[tokio::test]
    async fn other_params_are_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        for params in [
            json!({ "full": true }),
            json!([]),
            json!([1]),
            json!(null),
            json!("x"),
        ] {
            let error = handle(&store, Some(params.clone())).await.unwrap_err();
            assert_eq!(error, RpcError::invalid_params(), "params {params}");
        }
    }

    #[test]
    fn store_errors_become_a_fixed_internal_error() {
        let error = internal_error(StoreError::TaskFailed);
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert_eq!(error.message, "Internal error");
    }
}
