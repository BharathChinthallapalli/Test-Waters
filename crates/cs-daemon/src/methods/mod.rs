//! Control-API method handlers (feature 02 design, method table).
//!
//! [`Methods`] is the daemon's [`Handler`]: it holds the store, the control
//! token and the start time, and dispatches every method in
//! `cs_core::control::methods` to its handler: `health`, `version`,
//! `token.rotate` and `settings.*` in [`core`], `events.verify` in [`verify`],
//! `content.erasePlan` and `content.erase` in [`erase`], `calls.list` in
//! [`calls`]. Any other method is -32601. Each handler checks its own params (-32602) and maps its errors
//! (1001–1004, -32603) in one place.

pub mod calls;
pub mod core;
pub mod erase;
pub mod verify;

use std::sync::Arc;
use std::time::Instant;

use cs_core::control::methods;
use cs_store::Store;
use serde_json::Value;

use crate::rpc::{Handler, RpcError};
use crate::token::ControlToken;

/// Every control-API method, backed by the daemon's store and token.
pub struct Methods {
    store: Arc<Store>,
    token: Arc<ControlToken>,
    started: Instant,
}

impl Methods {
    /// `started` is when the daemon started; `health` reports the time since.
    pub fn new(store: Arc<Store>, token: Arc<ControlToken>, started: Instant) -> Self {
        Self {
            store,
            token,
            started,
        }
    }
}

impl Handler for Methods {
    async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        let store = &self.store;
        match method {
            methods::HEALTH => core::health(store, self.started, params).await,
            methods::VERSION => core::version(params),
            methods::TOKEN_ROTATE => core::rotate_token(&self.token, params).await,
            methods::SETTINGS_GET => core::settings_get(store, params),
            methods::SETTINGS_SET_CAPTURE_CONTENT => core::set_capture_content(store, params).await,
            methods::EVENTS_VERIFY => verify::handle(store, params).await,
            methods::CONTENT_ERASE_PLAN => erase::plan(store, params).await,
            methods::CONTENT_ERASE => erase::erase(store, params).await,
            methods::CALLS_LIST => calls::list(store, params).await,
            _ => Err(RpcError::method_not_found()),
        }
    }
}

#[cfg(test)]
mod tests {
    use cs_core::control::{HealthResult, HealthStatus, SettingsResult, VersionResult};
    use cs_core::rpc::codes;
    use cs_store::AppendEvent;
    use cs_store::migrate::CURRENT_SCHEMA_VERSION;
    use cs_store::secrets::{InMemorySecretStore, NO_SECRET_SERVICE, SecretStore};
    use serde_json::json;

    use super::*;
    use crate::http::TokenVerifier;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Arc<Store>,
        token: Arc<ControlToken>,
        methods: Methods,
    }

    fn fixture(secrets: impl SecretStore + 'static) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path(), Arc::new(secrets)).unwrap());
        let token = Arc::new(ControlToken::load_or_create(dir.path()).unwrap());
        let methods = Methods::new(Arc::clone(&store), Arc::clone(&token), Instant::now());
        Fixture {
            _dir: dir,
            store,
            token,
            methods,
        }
    }

    fn with_key() -> Fixture {
        fixture(InMemorySecretStore::new([9; 32]))
    }

    async fn append(store: &Store, run: &str, content: &[u8]) {
        store
            .append(AppendEvent {
                run_id: run.to_owned(),
                kind: "test.event".to_owned(),
                ts_ms: 1_790_000_000_000,
                body: json!({}),
                content: vec![content.to_vec()],
            })
            .await
            .unwrap();
    }

    async fn health(methods: &Methods) -> HealthResult {
        serde_json::from_value(methods.call("health", None).await.unwrap()).unwrap()
    }

    async fn settings(methods: &Methods) -> SettingsResult {
        serde_json::from_value(methods.call("settings.get", None).await.unwrap()).unwrap()
    }

    fn token_file(token: &ControlToken) -> String {
        std::fs::read_to_string(token.path()).unwrap()
    }

    #[tokio::test]
    async fn health_reports_a_fresh_store() {
        let f = with_key();

        let result = health(&f.methods).await;

        assert_eq!(result.status, HealthStatus::Ok);
        assert_eq!(result.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(!result.capture_content);
        assert_eq!(result.last_global_position, 0);
        assert!(!result.erasure_pending);
        assert!(result.uptime_ms < 60_000);
    }

    #[tokio::test]
    async fn health_follows_the_store() {
        let f = with_key();
        f.store.set_capture_content(true).await.unwrap();
        append(&f.store, "a", b"one").await;
        append(&f.store, "b", b"two").await;

        let result = health(&f.methods).await;

        assert!(result.capture_content);
        assert_eq!(result.last_global_position, 2);
        assert!(!result.erasure_pending);
        assert_eq!(result.erasure_pending, f.store.erasure_pending());
    }

    #[tokio::test]
    async fn uptime_counts_from_the_start_time_given() {
        let f = with_key();
        let started = Instant::now()
            .checked_sub(std::time::Duration::from_secs(5))
            .unwrap();
        let methods = Methods::new(Arc::clone(&f.store), Arc::clone(&f.token), started);

        assert!(health(&methods).await.uptime_ms >= 5_000);
    }

    #[tokio::test]
    async fn version_is_the_package_version() {
        let f = with_key();

        let value = f.methods.call("version", Some(json!({}))).await.unwrap();

        let result: VersionResult = serde_json::from_value(value).unwrap();
        assert_eq!(result.daemon_version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn settings_round_trip() {
        let f = with_key();

        assert!(!settings(&f.methods).await.capture_content);

        let on = f
            .methods
            .call(
                "settings.setCaptureContent",
                Some(json!({ "enabled": true })),
            )
            .await
            .unwrap();
        assert_eq!(on, json!({ "captureContent": true }));
        assert!(settings(&f.methods).await.capture_content);
        assert!(f.store.capture_content());

        let off = f
            .methods
            .call(
                "settings.setCaptureContent",
                Some(json!({ "enabled": false })),
            )
            .await
            .unwrap();
        assert_eq!(off, json!({ "captureContent": false }));
        assert!(!settings(&f.methods).await.capture_content);
    }

    #[tokio::test]
    async fn enabling_capture_without_a_keychain_is_1001_with_the_reason() {
        let f = fixture(InMemorySecretStore::unavailable(NO_SECRET_SERVICE));

        let error = f
            .methods
            .call(
                "settings.setCaptureContent",
                Some(json!({ "enabled": true })),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, codes::KEYCHAIN_UNAVAILABLE);
        assert_eq!(error.message, NO_SECRET_SERVICE);
        assert!(!f.store.capture_content());
        assert!(!health(&f.methods).await.capture_content);
    }

    #[tokio::test]
    async fn token_rotate_replaces_the_token_in_the_file_and_in_memory() {
        let f = with_key();
        let old = token_file(&f.token);

        let value = f.methods.call("token.rotate", None).await.unwrap();

        assert_eq!(value, json!({}));
        let new = token_file(&f.token);
        assert_ne!(new, old);
        assert!(!f.token.verify(old.as_bytes()));
        assert!(f.token.verify(new.as_bytes()));
    }

    #[tokio::test]
    async fn verify_and_erase_are_dispatched() {
        let f = with_key();
        f.store.set_capture_content(true).await.unwrap();
        append(&f.store, "a", b"shared").await;
        append(&f.store, "b", b"shared").await;

        let verified = f.methods.call("events.verify", None).await.unwrap();
        assert_eq!(
            verified,
            json!({ "ok": true, "eventsChecked": 2, "erasedEvents": 0 })
        );

        let unknown = f
            .methods
            .call("content.erasePlan", Some(json!({ "runId": "nope" })))
            .await
            .unwrap_err();
        assert_eq!(unknown.code, codes::UNKNOWN_RUN);

        let stale = f
            .methods
            .call(
                "content.erase",
                Some(json!({ "runId": "a", "planId": "0".repeat(64) })),
            )
            .await
            .unwrap_err();
        assert_eq!(stale.code, codes::ERASE_PLAN_OUT_OF_DATE);

        let plan = f
            .methods
            .call("content.erasePlan", Some(json!({ "runId": "a" })))
            .await
            .unwrap();
        assert_eq!(plan["sharedWithRuns"], json!(["b"]));
        let erased = f
            .methods
            .call(
                "content.erase",
                Some(json!({ "runId": "a", "planId": plan["planId"] })),
            )
            .await
            .unwrap();
        assert_eq!(erased["erasedItems"], 1);

        let after = health(&f.methods).await;
        assert!(!after.erasure_pending);
        assert_eq!(after.last_global_position, 3, "the content.erased event");
        let verified = f
            .methods
            .call("events.verify", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(verified["ok"], true);
        assert_eq!(verified["erasedEvents"], 2);
    }

    #[tokio::test]
    async fn calls_list_is_dispatched() {
        let f = with_key();
        append(&f.store, "a", b"not a call").await;

        let empty = f.methods.call("calls.list", None).await.unwrap();
        assert_eq!(empty, json!({ "calls": [] }));

        let bad = f
            .methods
            .call("calls.list", Some(json!({ "limit": 0 })))
            .await
            .unwrap_err();
        assert_eq!(bad, RpcError::invalid_params());
    }

    #[tokio::test]
    async fn methods_without_params_refuse_other_params() {
        let f = with_key();
        let old = token_file(&f.token);

        for method in [
            "health",
            "version",
            "token.rotate",
            "settings.get",
            "events.verify",
        ] {
            for params in [json!({ "x": 1 }), json!([]), json!([1])] {
                let error = f
                    .methods
                    .call(method, Some(params.clone()))
                    .await
                    .unwrap_err();
                assert_eq!(error, RpcError::invalid_params(), "{method} {params}");
            }
            let ok = f.methods.call(method, Some(json!({}))).await;
            assert!(ok.is_ok(), "{method} with {{}}: {ok:?}");
        }
        // Only the one call with `{}` rotated the token.
        assert!(!f.token.verify(old.as_bytes()));
    }

    #[tokio::test]
    async fn methods_with_params_refuse_missing_or_wrong_params() {
        let f = with_key();

        for (method, params) in [
            ("settings.setCaptureContent", None),
            ("settings.setCaptureContent", Some(json!({}))),
            (
                "settings.setCaptureContent",
                Some(json!({ "enabled": "yes" })),
            ),
            ("content.erasePlan", None),
            ("content.erasePlan", Some(json!({ "runId": 1 }))),
            ("content.erase", Some(json!({ "runId": "a" }))),
            // By position, or with unknown members: refused, not guessed at.
            ("settings.setCaptureContent", Some(json!([true]))),
            (
                "settings.setCaptureContent",
                Some(json!({ "enabled": true, "extra": 1 })),
            ),
            ("content.erasePlan", Some(json!(["a"]))),
            ("content.erase", Some(json!(["a", "0"]))),
        ] {
            let error = f.methods.call(method, params.clone()).await.unwrap_err();
            assert_eq!(error, RpcError::invalid_params(), "{method} {params:?}");
        }
        assert!(!f.store.capture_content());
    }

    #[tokio::test]
    async fn unknown_methods_are_32601() {
        let f = with_key();

        for method in ["", "Health", "health ", "rpc.discover", "content.eraseAll"] {
            let error = f.methods.call(method, None).await.unwrap_err();
            assert_eq!(error, RpcError::method_not_found(), "{method:?}");
        }
    }

    #[tokio::test]
    async fn store_failures_are_a_fixed_internal_error() {
        let f = with_key();
        f.store.close().await.unwrap();

        // Writes fail once the store is closed; the message names nothing.
        let error = f
            .methods
            .call(
                "settings.setCaptureContent",
                Some(json!({ "enabled": false })),
            )
            .await
            .unwrap_err();

        assert_eq!(error, RpcError::internal_error());
    }
}
