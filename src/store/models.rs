use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;

use crate::dsl::{CompiledDefinition, DefinitionKind};
use crate::{
    AgentCommandId, AgentExecutionView, AgentPolicyDecision, CatalogEpochId, FrameId, GoalContract,
    IdempotencyKey, KernelCommandPayload, PolicyRevision, PrincipalId, RunId, RuntimeCeilings,
    RuntimeCommandKey, RuntimeTransition, SessionId, StartAgent, TaskPreferences, TaskTarget,
    UserTaskId,
};

#[derive(Clone, Debug)]
pub struct CreateSession {
    pub principal: PrincipalId,
    pub working_directory: String,
    pub retention_until: time::OffsetDateTime,
}

/// Deployment-owned ceilings enforced transactionally by `PostgresStore`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreLimits {
    pub max_active_sessions_per_principal: usize,
    pub max_active_runs_per_session: usize,
    pub max_open_permission_requests_per_session: usize,
    pub agent_runtime: RuntimeCeilings,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_active_sessions_per_principal: usize::MAX,
            max_active_runs_per_session: usize::MAX,
            max_open_permission_requests_per_session: usize::MAX,
            agent_runtime: RuntimeCeilings::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentCommandState {
    Pending,
    Claimed,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct AgentCommandView {
    pub id: AgentCommandId,
    pub task_id: UserTaskId,
    pub key: RuntimeCommandKey,
    pub ordinal: u16,
    pub schema_version: u16,
    pub payload: KernelCommandPayload,
    pub payload_digest: String,
    pub state: AgentCommandState,
    pub revision: u64,
    pub claimant: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AgentCommandContext {
    pub execution: AgentExecutionView,
    pub definition: CompiledDefinition,
    pub command: AgentCommandView,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentRecoveryReport {
    pub pending: u64,
    pub claimed: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentTaskEventKind {
    AgentStarted,
    CommandClaimed,
    RuntimeTransitionCommitted,
    RuntimeCommandUnavailable,
    GoalCompleted,
    GoalBlocked,
    TaskPaused,
    TaskResumed,
    TaskCancelled,
    TaskFailed,
    TaskSucceeded,
}

impl AgentTaskEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentStarted => "agent.started",
            Self::CommandClaimed => "command.claimed",
            Self::RuntimeTransitionCommitted => "runtime.transition_committed",
            Self::RuntimeCommandUnavailable => "runtime.command_unavailable",
            Self::GoalCompleted => "goal.completed",
            Self::GoalBlocked => "goal.blocked",
            Self::TaskPaused => "task.paused",
            Self::TaskResumed => "task.resumed",
            Self::TaskCancelled => "task.cancelled",
            Self::TaskFailed => "task.failed",
            Self::TaskSucceeded => "task.succeeded",
        }
    }

    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "agent.started" => Ok(Self::AgentStarted),
            "command.claimed" => Ok(Self::CommandClaimed),
            "runtime.transition_committed" => Ok(Self::RuntimeTransitionCommitted),
            "runtime.command_unavailable" => Ok(Self::RuntimeCommandUnavailable),
            "goal.completed" => Ok(Self::GoalCompleted),
            "goal.blocked" => Ok(Self::GoalBlocked),
            "task.paused" => Ok(Self::TaskPaused),
            "task.resumed" => Ok(Self::TaskResumed),
            "task.cancelled" => Ok(Self::TaskCancelled),
            "task.failed" => Ok(Self::TaskFailed),
            "task.succeeded" => Ok(Self::TaskSucceeded),
            _ => Err(StoreError::InvalidData(
                "agent task event kind is malformed".to_owned(),
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentTaskEvent {
    pub sequence: u64,
    pub kind: AgentTaskEventKind,
    pub metadata: serde_json::Value,
    pub task_revision: u64,
    pub goal_revision: u64,
    pub created_at: time::OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct AgentStartSeed {
    pub execution: AgentExecutionView,
    pub definition: CompiledDefinition,
}

pub struct AgentStartCommit<'a> {
    pub principal: &'a PrincipalId,
    pub request: &'a StartAgent,
    pub frozen_definition_id: uuid::Uuid,
    pub frozen_definition_hash: &'a str,
    pub policy: &'a AgentPolicyDecision,
    pub transition: &'a RuntimeTransition,
    pub ceilings: &'a RuntimeCeilings,
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
    pub kind: DefinitionKind,
    pub name: String,
    pub version: u32,
    pub definition_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedUserTask {
    pub id: UserTaskId,
    pub session_id: SessionId,
    pub principal: PrincipalId,
    pub definition: PublishedDefinition,
    pub target: TaskTarget,
    pub preferences: TaskPreferences,
    pub goal: GoalContract,
    pub goal_digest: String,
    pub revision: u64,
    pub created_at: time::OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Clone, Debug)]
pub struct SuspensionFrame {
    pub frame_id: FrameId,
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct PendingPermission {
    pub tool_call_id: uuid::Uuid,
    pub frame_id: FrameId,
    pub revision: u64,
    pub provider_call_id: String,
    pub qualified_name: String,
    pub descriptor_digest: String,
    pub arguments: Value,
    pub resolved: bool,
    pub resolution_idempotency_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RunEvent {
    pub sequence: u64,
    pub kind: String,
    pub metadata: Value,
    pub created_at: time::OffsetDateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    Runnable,
    Running,
    WaitingForAnswer,
    WaitingForPermission,
    OutcomeUnknown,
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
pub struct RecoverableToolCall {
    pub id: uuid::Uuid,
    pub frame_id: FrameId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub revision: u64,
    pub qualified_name: String,
    pub descriptor_digest: String,
    pub idempotency_key: IdempotencyKey,
    pub arguments: Value,
    pub recovery: crate::RecoverySupport,
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
    Goto {
        task_name: String,
        bindings: Bindings,
    },
    Complete(Value),
    Fail {
        code: String,
        message: String,
    },
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
    #[error("user task was not found")]
    UserTaskNotFound,
    #[error("user task requires a published agent definition")]
    AgentDefinitionRequired,
    #[error("user task request is invalid: {0}")]
    InvalidUserTask(String),
    #[error("agent definitions cannot start through the workflow runner")]
    AgentRuntimeUnavailable,
    #[error("agent transition is invalid: {0}")]
    AgentTransitionInvalid(String),
    #[error("agent task has already started or terminated")]
    AgentTaskAlreadyTerminal,
    #[error("frame was not found")]
    FrameNotFound,
    #[error("execution revision conflict")]
    RevisionConflict,
    #[error("deployment quota exceeded: {0}")]
    QuotaExceeded(&'static str),
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
    #[error("runtime state is invalid: {0}")]
    RuntimeStateInvalid(String),
}
