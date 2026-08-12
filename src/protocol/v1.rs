use serde_json::Value;

use super::{ApplicationCommand, ProtocolError};

/// Stable ACP V1 extension adapter. The surrounding ACP server owns session lifecycle;
/// this adapter only accepts Kiteframe's namespaced workflow commands.
pub struct V1Adapter;

impl V1Adapter {
    pub fn decode_extension(method: &str, payload: Value) -> Result<ApplicationCommand, ProtocolError> {
        match method {
            "_kiteframe/run/start" => decode(payload, ApplicationCommand::Start),
            "_kiteframe/run/get" => decode(payload, ApplicationCommand::Get),
            "_kiteframe/run/answer" => decode(payload, ApplicationCommand::Answer),
            "_kiteframe/run/resolve-permission" => decode(payload, ApplicationCommand::ResolvePermission),
            "_kiteframe/run/cancel" => decode(payload, ApplicationCommand::Cancel),
            "_kiteframe/run/events" => decode(payload, ApplicationCommand::ReplayEvents),
            _ => Err(ProtocolError::Unsupported),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(payload: Value, map: impl FnOnce(T) -> ApplicationCommand) -> Result<ApplicationCommand, ProtocolError> {
    serde_json::from_value(payload).map(map).map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
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
        assert!(matches!(V1Adapter::decode_extension("_kiteframe/run/get", json!({"run_id":"019ff3d8-9d86-7de0-9070-0f0185d5f413"})), Ok(ApplicationCommand::Get(_))));
        assert!(V1Adapter::decode_extension("tool/call", json!({})).is_err());
    }
}
