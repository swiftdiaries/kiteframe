use serde_json::Value;

use super::{ApplicationCommand, ProtocolError};

/// Stable ACP V1 extension adapter. The surrounding ACP server owns session lifecycle;
/// this adapter only accepts Kiteframe's namespaced workflow commands.
pub struct V1Adapter;

impl V1Adapter {
    pub fn decode_extension(method: &str, payload: Value) -> Result<ApplicationCommand, ProtocolError> {
        match method {
            "_kiteframe/run/start" => serde_json::from_value(payload)
                .map(ApplicationCommand::Start)
                .map_err(|error| ProtocolError::InvalidPayload(error.to_string())),
            _ => Err(ProtocolError::Unsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ApplicationCommand;
    use serde_json::json;

    use super::V1Adapter;

    #[test]
    fn decodes_namespaced_run_start_only() {
        let command = V1Adapter::decode_extension("_kiteframe/run/start", json!({
            "session_id": "019ff3d8-9d86-7de0-9070-0f0185d5f413",
            "workflow_name": "w", "version": null, "input": {}, "idempotency_key": "start"
        })).expect("start command");
        assert!(matches!(command, ApplicationCommand::Start(_)));
        assert!(V1Adapter::decode_extension("tool/call", json!({})).is_err());
    }
}
