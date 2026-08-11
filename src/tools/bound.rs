use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    CatalogEpochId, FrameId, HashError, IdempotencyKey, PolicyRevision, RunId, SchemaError,
    SessionId,
};

use super::CatalogTool;

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub frame_id: FrameId,
    pub policy_revision: PolicyRevision,
    arguments: ValidatedArguments,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug)]
pub struct ValidatedArguments(Value);

impl ValidatedArguments {
    pub fn for_tool(tool: &CatalogTool, arguments: Value) -> Result<Self, ToolError> {
        tool.input()
            .validate(&arguments)
            .map_err(ToolError::InvalidArguments)?;
        Ok(Self(arguments))
    }

    #[must_use]
    pub const fn as_json(&self) -> &Value {
        &self.0
    }
}

impl ToolCall {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        run_id: RunId,
        frame_id: FrameId,
        policy_revision: PolicyRevision,
        tool: &CatalogTool,
        arguments: Value,
        idempotency_key: IdempotencyKey,
    ) -> Result<Self, ToolError> {
        Ok(Self {
            session_id,
            run_id,
            frame_id,
            policy_revision,
            arguments: ValidatedArguments::for_tool(tool, arguments)?,
            idempotency_key,
        })
    }

    #[must_use]
    pub const fn arguments(&self) -> &ValidatedArguments {
        &self.arguments
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    WaitingForPermission,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolFailure {
    ToolContractChanged,
    WaitingForPermission,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid qualified tool name: {0}")]
    InvalidQualifiedName(String),
    #[error("duplicate qualified tool name: {0}")]
    DuplicateQualifiedName(String),
    #[error("unsupported closed schema: {0}")]
    UnsupportedSchema(#[source] SchemaError),
    #[error("unsafe recovery support for effectful tool: {0}")]
    UnsafeEffectRecovery(String),
    #[error("tool contract changed: {qualified_name}")]
    ToolContractChanged { qualified_name: String },
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("unknown session")]
    UnknownSession(SessionId),
    #[error("catalog epoch mismatch")]
    CatalogEpochMismatch(CatalogEpochId),
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(#[source] SchemaError),
    #[error("canonical descriptor error: {0}")]
    Canonicalization(#[source] HashError),
    #[error("tool invocation failed")]
    InvocationFailed,
}

#[async_trait]
pub trait BoundTool: Send + Sync {
    fn descriptor(&self) -> &CatalogTool;
    async fn invoke(&self, call: ToolCall) -> Result<Value, ToolError>;
    async fn status(&self, key: &IdempotencyKey) -> Result<ToolStatus, ToolError>;
}

pub trait ToolResolver: Send + Sync {
    fn bound_tool(
        &self,
        session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<Arc<dyn BoundTool>, ToolError>;
}
