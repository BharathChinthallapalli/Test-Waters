//! Wire types of the daemon's JSON-RPC control API (ADR 0003), and the
//! discovery record clients read to find the daemon.

use std::net::SocketAddrV4;

use serde::{Deserialize, Serialize};

/// The contents of `daemon.json` in the data directory: where a running daemon
/// listens. Written owner-only once the listener is bound, removed on graceful
/// shutdown (feature 02 design, "Single instance and discovery").
///
/// Clients send the token to `address` only while the daemon holds
/// `daemon.lock` and, on Unix, the pid in `daemon.lock` equals `pid`. Readers
/// ignore unknown fields, so a later daemon can add some.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    /// The daemon's process id.
    pub pid: u32,
    /// When the daemon started, in milliseconds since the Unix epoch.
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at_ms: u64,
    /// Always `127.0.0.1:<port>`, the bound port (never 0). Connect to exactly
    /// this address, never to `localhost` (R2.7).
    #[cfg_attr(test, ts(type = "string"))]
    pub address: SocketAddrV4,
    /// The schema version of the running daemon's store.
    pub schema_version: u32,
}

/// Result of the `version` method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct VersionResult {
    /// Version of the running daemon, from its Cargo package.
    pub daemon_version: String,
}

/// Method names, as sent in a request's `method` member.
pub mod methods {
    pub const HEALTH: &str = "health";
    pub const VERSION: &str = "version";
    pub const TOKEN_ROTATE: &str = "token.rotate";
    pub const SETTINGS_GET: &str = "settings.get";
    pub const SETTINGS_SET_CAPTURE_CONTENT: &str = "settings.setCaptureContent";
    pub const EVENTS_VERIFY: &str = "events.verify";
    pub const CONTENT_ERASE_PLAN: &str = "content.erasePlan";
    pub const CONTENT_ERASE: &str = "content.erase";
}

/// Params of methods that take none. Clients send `{}` or omit `params`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct NoParams {}

/// `status` of a `health` result. The daemon answers only while it is healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub enum HealthStatus {
    Ok,
}

/// Result of the `health` method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct HealthResult {
    pub status: HealthStatus,
    #[cfg_attr(test, ts(type = "number"))]
    pub uptime_ms: u64,
    pub schema_version: u32,
    pub capture_content: bool,
    /// Global commit position of the newest event, 0 for an empty log.
    #[cfg_attr(test, ts(type = "number"))]
    pub last_global_position: u64,
    /// An erasure deleted content but a copy may remain until a retry succeeds.
    pub erasure_pending: bool,
}

/// Result of `token.rotate`. The new token is in the token file, never in a response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TokenRotateResult {}

/// Result of `settings.get` and `settings.setCaptureContent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct SettingsResult {
    pub capture_content: bool,
}

/// Params of `settings.setCaptureContent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct SetCaptureContentParams {
    pub enabled: bool,
}

/// What verification found wrong first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub enum VerifyProblemKind {
    /// A gap or repeat in the global commit order.
    GlobalPositionGap,
    /// A gap or repeat in a run's sequence numbers.
    SequenceGap,
    /// `prevHash` doesn't match the previous event's hash.
    PrevHashMismatch,
    /// The recomputed hash differs: a field was edited.
    EventHashMismatch,
    /// The run's recorded head disagrees with its last event.
    RunHeadMismatch,
    /// Referenced content is missing and no erasure event explains it.
    ContentMissing,
}

/// The first problem verification found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct VerifyProblem {
    pub kind: VerifyProblemKind,
    #[cfg_attr(test, ts(type = "number"))]
    pub global_pos: u64,
    pub run_id: String,
}

/// Result of `events.verify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub ok: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub events_checked: u64,
    /// Events whose content was erased on purpose; these still verify.
    #[cfg_attr(test, ts(type = "number"))]
    pub erased_events: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub first_problem: Option<VerifyProblem>,
}

/// Params of `content.erasePlan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct ErasePlanParams {
    pub run_id: String,
}

/// Result of `content.erasePlan`: what an erase would remove. Erases nothing (R6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct ErasePlanResult {
    pub plan_id: String,
    pub run_id: String,
    /// Other runs that refer to some of the same content and lose it too.
    pub shared_with_runs: Vec<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub content_items: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub backups_to_remove: u64,
}

/// Params of `content.erase`: the plan the user confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct EraseParams {
    pub run_id: String,
    pub plan_id: String,
}

/// Result of `content.erase`. Returned only once every copy is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct EraseResult {
    #[cfg_attr(test, ts(type = "number"))]
    pub erased_items: u64,
    pub affected_runs: Vec<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub backups_removed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_result_uses_camel_case_on_the_wire() {
        let result = VersionResult {
            daemon_version: "0.1.0".to_owned(),
        };

        let json = serde_json::to_value(&result).unwrap();

        assert_eq!(json, serde_json::json!({ "daemonVersion": "0.1.0" }));
        assert_eq!(
            serde_json::from_value::<VersionResult>(json).unwrap(),
            result
        );
    }

    #[test]
    fn verify_result_omits_absent_problem_and_names_kinds_in_camel_case() {
        let clean = VerifyResult {
            ok: true,
            events_checked: 3,
            erased_events: 0,
            first_problem: None,
        };
        assert_eq!(
            serde_json::to_value(&clean).unwrap(),
            serde_json::json!({ "ok": true, "eventsChecked": 3, "erasedEvents": 0 })
        );

        let problem = VerifyProblem {
            kind: VerifyProblemKind::PrevHashMismatch,
            global_pos: 2,
            run_id: "r".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&problem).unwrap(),
            serde_json::json!({ "kind": "prevHashMismatch", "globalPos": 2, "runId": "r" })
        );
    }

    #[test]
    fn methods_without_params_accept_an_empty_object() {
        assert_eq!(
            serde_json::from_value::<NoParams>(serde_json::json!({})).unwrap(),
            NoParams {}
        );
    }
}
