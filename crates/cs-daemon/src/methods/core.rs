//! `health`, `version`, `token.rotate`, `settings.get` and
//! `settings.setCaptureContent`. Owned by unit `wire`.
//!
//! Every method here except `settings.setCaptureContent` takes no params:
//! `params` absent or `{}`, anything else is -32602 ([`no_params`]). Methods
//! with params take them by name only ([`object_params`]).
//! Errors the client can't fix are -32603 with a fixed message; the cause goes
//! to the log, and neither ever contains a token, a request or content.
//!
//! **Cancellation** (see [`crate::rpc::Handler`]): `token.rotate` runs on a
//! blocking thread that finishes even when the request is dropped, so the
//! token file and the token in memory never disagree. Enabling capture waits
//! for the keychain on the store's blocking thread; if the request is dropped
//! before that returns, capture stays off (`cs_store::writer`, "The capture
//! setting and the content key").

use std::sync::Arc;
use std::time::Instant;

use cs_core::control::{
    HealthResult, HealthStatus, SetCaptureContentParams, SettingsResult, TokenRotateResult,
    VersionResult,
};
use cs_store::migrate::CURRENT_SCHEMA_VERSION;
use cs_store::{Store, StoreError};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::rpc::RpcError;
use crate::token::ControlToken;

/// `health`: the daemon answers, and the store's state.
pub async fn health(
    store: &Store,
    started: Instant,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    no_params(params)?;
    let last_global_position = store
        .last_global_position()
        .await
        .map_err(|error| store_failure("health", &error))?;
    to_value(&HealthResult {
        status: HealthStatus::Ok,
        uptime_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        schema_version: CURRENT_SCHEMA_VERSION,
        capture_content: store.capture_content(),
        last_global_position,
        erasure_pending: store.erasure_pending(),
    })
}

/// `version`: the daemon's Cargo package version (R2.5).
pub fn version(params: Option<Value>) -> Result<Value, RpcError> {
    no_params(params)?;
    to_value(&VersionResult {
        daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

/// `token.rotate` (R2.9): writes a new token to the token file, then rejects
/// the old one. The new token is never in the response.
pub async fn rotate_token(
    token: &Arc<ControlToken>,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    no_params(params)?;
    let token = Arc::clone(token);
    let rotated = tokio::task::spawn_blocking(move || token.rotate())
        .await
        .map_err(|_| {
            tracing::error!("token rotation did not finish");
            RpcError::internal_error()
        })?;
    if let Err(error) = rotated {
        // `TokenError` names the file, never a token; the old token stays valid.
        tracing::error!(%error, "token rotation failed");
        return Err(RpcError::internal_error());
    }
    tracing::info!("control token rotated");
    to_value(&TokenRotateResult {})
}

/// `settings.get`.
pub fn settings_get(store: &Store, params: Option<Value>) -> Result<Value, RpcError> {
    no_params(params)?;
    to_value(&SettingsResult {
        capture_content: store.capture_content(),
    })
}

/// `settings.setCaptureContent` ([`SetCaptureContentParams`]). Enabling without
/// a usable keychain fails with 1001 and the store's reason for the user, and
/// capture stays off (R5.4).
pub async fn set_capture_content(store: &Store, params: Option<Value>) -> Result<Value, RpcError> {
    let params: SetCaptureContentParams = object_params(params)?;
    let capture_content = store
        .set_capture_content(params.enabled)
        .await
        .map_err(|error| match error {
            // The reason is a fixed text written for the user, never the key.
            StoreError::Keychain(unavailable) => {
                RpcError::new(unavailable.code(), unavailable.reason())
            }
            other => store_failure("settings.setCaptureContent", &other),
        })?;
    to_value(&SettingsResult { capture_content })
}

/// Params given by name: a JSON object with exactly the type's members (the
/// params types deny unknown fields). Absent params, by-position params (an
/// array) and anything that doesn't fit are -32602.
pub fn object_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    match params {
        Some(params @ Value::Object(_)) => {
            serde_json::from_value(params).map_err(|_| RpcError::invalid_params())
        }
        _ => Err(RpcError::invalid_params()),
    }
}

/// Accepts absent params or `{}`; anything else is -32602.
pub fn no_params(params: Option<Value>) -> Result<(), RpcError> {
    match params {
        None => Ok(()),
        Some(Value::Object(members)) if members.is_empty() => Ok(()),
        Some(_) => Err(RpcError::invalid_params()),
    }
}

/// Logs a store failure and gives the client a fixed message only. Store
/// errors name no content, token or request.
fn store_failure(method: &'static str, error: &StoreError) -> RpcError {
    tracing::error!(method, %error, "store operation failed");
    RpcError::internal_error()
}

fn to_value(result: &impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(result).map_err(|_| RpcError::internal_error())
}
