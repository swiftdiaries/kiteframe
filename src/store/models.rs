use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;

use crate::{CatalogEpochId, FrameId, IdempotencyKey, PolicyRevision, PrincipalId, RunId, SessionId};

#[derive(Clone, Debug)]
pub struct CreateSession {
    pub principal: PrincipalId,
    pub working_directory: String,
    pub retention_until: time::OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionView {
    pub id: SessionId,
    pub principal: PrincipalId,
    pub policy_revision: PolicyRevision,
    pub current_catalog_epoch: Option<CatalogEpochId>,
    pub mcp_attached: bool,
}

/// Persistence-facing representation until Task 8 owns typed policy validation.
#[derive(Clone, Debug)]
pub struct PolicyUpdate {
    pub session: SessionId,
    pub expected_revision: PolicyRevision,
    pub idempotency_key: String,
    pub typed_source: Value,
    pub compiled_cedar: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedDefinition {
    pub id: uuid::Uuid,
    pub name: String,
    pub version: u32,
    pub definition_hash: String,
}

#[derive(Clone, Debug)]
pub struct StartRun {
    pub session_id: SessionId,
    pub workflow_name: String,
    pub version: Option<u32>,
    pub input: Value,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunView {
    pub id: RunId,
    pub session_id: SessionId,
    pub definition: PublishedDefinition,
    pub root_frame_id: FrameId,
    pub state: RunState,
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct RunnableFrame {
    pub id: FrameId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub task_name: String,
    pub outcome_name: Option<String>,
    pub statement_index: usize,
    pub bindings: Value,
    pub revision: u64,
    pub workflow: crate::dsl::CompiledWorkflow,
}

#[derive(Clone, Debug)]
pub struct RunToolBinding {
    pub qualified_name: String,
    pub descriptor_digest: String,
    pub capabilities: Vec<crate::Capability>,
    pub descriptor: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    Runnable,
    Running,
    WaitingForAnswer,
    WaitingForPermission,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelAttempt {
    pub frame_id: FrameId,
    pub attempt: u32,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCallIntent {
    pub id: uuid::Uuid,
    pub frame_id: FrameId,
    pub provider_call_id: String,
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct NewPermissionRequest {
    pub id: crate::PermissionRequestId,
    pub provider_call_id: String,
    pub qualified_tool: String,
    pub descriptor_digest: String,
    pub input_digest: String,
    pub policy_revision: PolicyRevision,
}

#[derive(Clone, Debug)]
pub struct Question {
    pub id: uuid::Uuid,
    pub binding: String,
    pub text: String,
    pub schema: crate::ClosedSchema,
}

#[derive(Clone, Debug)]
pub struct NewChild {
    pub target: String,
    pub binding: String,
    pub arguments: Bindings,
    pub branch_index: i32,
}

pub type Bindings = BTreeMap<String, Value>;

#[derive(Clone, Debug)]
pub enum FrameCommand {
    StartCall(NewChild),
    StartParallel(Vec<NewChild>),
    SuspendAsk(Question),
    Goto { task_name: String, bindings: Bindings },
    Complete(Value),
    Fail { code: String, message: String },
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("store migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("session was not found")]
    SessionNotFound,
    #[error("session principal does not match")]
    PrincipalMismatch,
    #[error("session is closed")]
    SessionClosed,
    #[error("conflicting idempotent request")]
    IdempotencyConflict,
    #[error("policy revision conflict")]
    PolicyRevisionConflict,
    #[error("workflow references a catalog binding outside the current session catalog")]
    CatalogScopeViolation,
    #[error("workflow definition was not found")]
    DefinitionNotFound,
    #[error("frame was not found")]
    FrameNotFound,
    #[error("execution revision conflict")]
    RevisionConflict,
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
}
