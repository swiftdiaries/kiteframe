use serde_json::Value;

use super::{ApplicationCommand, ProtocolError};

/// Draft ACP V2 adapter. This module is compiled only when the ACP V2 feature is enabled.
pub struct V2Adapter;

impl V2Adapter {
    pub fn decode_extension(method: &str, payload: Value) -> Result<ApplicationCommand, ProtocolError> {
        super::v1::V1Adapter::decode_extension(method, payload)
    }
}
