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

mod private {
    pub trait Sealed {}
}

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
pub struct ValidatedArguments {
    target_qualified_name: String,
    target_descriptor_digest: String,
    value: Value,
}

impl ValidatedArguments {
    pub fn for_tool(tool: &CatalogTool, arguments: Value) -> Result<Self, ToolError> {
        tool.verify_descriptor_digest()?;
        tool.input()
            .validate(&arguments)
            .map_err(ToolError::InvalidArguments)?;
        Ok(Self {
            target_qualified_name: tool.qualified_name().to_owned(),
            target_descriptor_digest: tool.descriptor_digest().to_owned(),
            value: arguments,
        })
    }

    #[must_use]
    pub const fn as_json(&self) -> &Value {
        &self.value
    }

    fn verify_target(&self, tool: &CatalogTool) -> Result<(), ToolError> {
        tool.verify_descriptor_digest()?;
        if self.target_qualified_name == tool.qualified_name()
            && self.target_descriptor_digest == tool.descriptor_digest()
        {
            Ok(())
        } else {
            Err(ToolError::ToolContractChanged {
                qualified_name: tool.qualified_name().to_owned(),
            })
        }
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

    pub(crate) fn verify_target(&self, tool: &CatalogTool) -> Result<(), ToolError> {
        self.arguments.verify_target(tool)
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
    #[error("invalid stored tool descriptor: {0}")]
    InvalidStoredDescriptor(String),
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
/// Deployment-owned implementation hook. Use [`bind_tool`] before registration so calls pass
/// through the descriptor-identity boundary.
pub trait TrustedTool: Send + Sync {
    fn descriptor(&self) -> &CatalogTool;
    async fn invoke(&self, call: ToolCall) -> Result<Value, ToolError>;
    async fn status(&self, key: &IdempotencyKey) -> Result<ToolStatus, ToolError>;
}

#[async_trait]
#[allow(private_bounds)]
pub trait BoundTool: private::Sealed + Send + Sync {
    fn descriptor(&self) -> &CatalogTool;
    async fn invoke(&self, call: ToolCall) -> Result<Value, ToolError>;
    async fn status(&self, key: &IdempotencyKey) -> Result<ToolStatus, ToolError>;
}

struct ValidatedBoundTool {
    implementation: Arc<dyn TrustedTool>,
}

impl private::Sealed for ValidatedBoundTool {}

#[async_trait]
impl BoundTool for ValidatedBoundTool {
    fn descriptor(&self) -> &CatalogTool {
        self.implementation.descriptor()
    }

    async fn invoke(&self, call: ToolCall) -> Result<Value, ToolError> {
        call.verify_target(self.descriptor())?;
        self.implementation.invoke(call).await
    }

    async fn status(&self, key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        self.implementation.status(key).await
    }
}

/// Wraps a deployment-owned tool implementation in the checked invocation boundary.
#[must_use]
pub fn bind_tool(implementation: Arc<dyn TrustedTool>) -> Arc<dyn BoundTool> {
    Arc::new(ValidatedBoundTool { implementation })
}

#[async_trait]
pub trait ToolResolver: Send + Sync {
    fn bound_tool(
        &self,
        session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<Arc<dyn BoundTool>, ToolError>;

    async fn invoke(
        &self,
        session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
        call: ToolCall,
    ) -> Result<Value, ToolError>;
}
