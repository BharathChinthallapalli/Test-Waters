//! Wire types of the daemon's JSON-RPC control API (ADR 0003).

use serde::{Deserialize, Serialize};

/// Result of the `version` method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub struct VersionResult {
    /// Version of the running daemon, from its Cargo package.
    pub daemon_version: String,
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
}
