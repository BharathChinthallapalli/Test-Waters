//! `content.erasePlan` and `content.erase`. Owned by unit `erase`.
//!
//! Thin handlers over [`Store::erase_plan`] and [`Store::erase`]
//! (`cs_store::erase` has the steps). Store errors map to JSON-RPC errors here:
//! 1003 unknown run, 1002 plan out of date, 1004 erasure pending, anything else
//! -32603. Messages are `EraseError`'s fixed texts: they never echo the run ID, a content
//! address or content. Unit `wire` registers the handlers.

use cs_core::control::{EraseParams, ErasePlanParams};
use cs_core::rpc::codes;
use cs_store::{EraseError, Store};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::rpc::RpcError;

/// `content.erasePlan` (params [`ErasePlanParams`]): the dry run. Erases nothing.
pub async fn plan(store: &Store, params: Option<Value>) -> Result<Value, RpcError> {
    let params: ErasePlanParams = parse(params)?;
    let result = store.erase_plan(&params.run_id).await.map_err(rpc_error)?;
    to_value(&result)
}

/// `content.erase` (params [`EraseParams`]): erases the run's content if the
/// plan the user confirmed is still current.
pub async fn erase(store: &Store, params: Option<Value>) -> Result<Value, RpcError> {
    let params: EraseParams = parse(params)?;
    let result = store
        .erase(&params.run_id, &params.plan_id)
        .await
        .map_err(rpc_error)?;
    to_value(&result)
}

fn parse<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    let params = params.ok_or_else(RpcError::invalid_params)?;
    serde_json::from_value(params).map_err(|_| RpcError::invalid_params())
}

fn to_value(result: &impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(result).map_err(|_| RpcError::internal_error())
}

/// The three Callsheet errors carry `EraseError`'s own message, which is a
/// fixed text.
fn rpc_error(error: EraseError) -> RpcError {
    match error {
        EraseError::UnknownRun => RpcError::new(codes::UNKNOWN_RUN, error.to_string()),
        EraseError::PlanOutOfDate => {
            RpcError::new(codes::ERASE_PLAN_OUT_OF_DATE, error.to_string())
        }
        EraseError::Pending => RpcError::new(codes::ERASURE_PENDING, error.to_string()),
        other @ (EraseError::Store(_) | EraseError::Io(_)) => {
            // Store and I/O errors name no content (see `cs_store::erase`).
            tracing::warn!(error = %other, "content erasure failed");
            RpcError::internal_error()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cs_store::AppendEvent;
    use cs_store::secrets::InMemorySecretStore;
    use serde_json::json;

    use super::*;

    async fn store_with_shared_content(dir: &std::path::Path) -> Store {
        let store = Store::open(dir, Arc::new(InMemorySecretStore::new([3; 32]))).unwrap();
        store.set_capture_content(true).await.unwrap();
        for (n, run) in ["a", "b"].into_iter().enumerate() {
            store
                .append(AppendEvent {
                    run_id: run.to_owned(),
                    kind: "test.event".to_owned(),
                    ts_ms: 1_790_000_000_000 + u64::try_from(n).unwrap(),
                    body: json!({}),
                    content: vec![b"secret message".to_vec()],
                })
                .await
                .unwrap();
        }
        store
    }

    #[tokio::test]
    async fn plan_then_erase_round_trips_on_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_shared_content(dir.path()).await;

        let planned = plan(&store, Some(json!({ "runId": "a" }))).await.unwrap();
        assert_eq!(planned["runId"], "a");
        assert_eq!(planned["sharedWithRuns"], json!(["b"]));
        assert_eq!(planned["contentItems"], 1);
        assert_eq!(planned["backupsToRemove"], 0);
        let plan_id = planned["planId"].as_str().unwrap().to_owned();

        let erased = erase(&store, Some(json!({ "runId": "a", "planId": plan_id })))
            .await
            .unwrap();
        assert_eq!(
            erased,
            json!({ "erasedItems": 1, "affectedRuns": ["b"], "backupsRemoved": 0 })
        );
        assert_eq!(store.blob_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn store_errors_map_to_callsheet_codes_without_echoing_input() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_shared_content(dir.path()).await;
        let secret_run = "run-name-that-must-not-be-echoed";

        let unknown = plan(&store, Some(json!({ "runId": secret_run })))
            .await
            .unwrap_err();
        assert_eq!(unknown.code, codes::UNKNOWN_RUN);
        let unknown = erase(
            &store,
            Some(json!({ "runId": secret_run, "planId": "0".repeat(64) })),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.code, codes::UNKNOWN_RUN);
        assert!(!unknown.message.contains(secret_run));

        let stale_id = "f".repeat(64);
        let stale = erase(&store, Some(json!({ "runId": "a", "planId": stale_id })))
            .await
            .unwrap_err();
        assert_eq!(stale.code, codes::ERASE_PLAN_OUT_OF_DATE);
        assert!(!stale.message.contains(&stale_id));
        assert_eq!(store.blob_count().await.unwrap(), 1, "nothing erased");

        let pending = rpc_error(EraseError::Pending);
        assert_eq!(pending.code, codes::ERASURE_PENDING);
        assert!(pending.message.starts_with("erasure pending"));
    }

    #[tokio::test]
    async fn bad_params_are_invalid_params() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_shared_content(dir.path()).await;

        for params in [None, Some(json!({})), Some(json!({ "runId": 7 }))] {
            let error = plan(&store, params).await.unwrap_err();
            assert_eq!(error.code, codes::INVALID_PARAMS);
        }
        for params in [
            None,
            Some(json!({ "runId": "a" })),
            Some(json!({ "planId": "x" })),
        ] {
            let error = erase(&store, params).await.unwrap_err();
            assert_eq!(error.code, codes::INVALID_PARAMS);
        }
        assert_eq!(store.blob_count().await.unwrap(), 1);
    }
}
