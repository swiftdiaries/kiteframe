use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;

use crate::dsl::{CompiledDefinition, DefinitionKind};
use crate::harness::{
    HARNESS_PROTOCOL_VERSION, HarnessApprovalDecision, HarnessInvocationEvent, HarnessInvocationId,
    HarnessProfileId, HarnessTransport,
};
use crate::{
    AgentCommandId, AgentExecutionView, AgentPolicyDecision, CatalogEpochId, FrameId, GoalContract,
    IdempotencyKey, KernelCommandPayload, PolicyRevision, PrincipalId, RunId, RuntimeCeilings,
    RuntimeCommandKey, RuntimeTransition, SessionId, StartAgent, TaskPreferences, TaskTarget,
    UserTaskId, canonical_sha256,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessPluginRegistration {
    pub profile_id: HarnessProfileId,
    pub profile_digest: String,
    pub source_repository: String,
    pub source_revision: String,
    pub transport: HarnessTransport,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HarnessInvocationState {
    Prepared,
    Started,
    Accepted,
    Completed,
    Cancelled,
    Failed,
    OutcomeUnknown,
}

impl HarnessInvocationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Started => "started",
            Self::Accepted => "accepted",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }

    pub fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "started" => Ok(Self::Started),
            "accepted" => Ok(Self::Accepted),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            "outcome_unknown" => Ok(Self::OutcomeUnknown),
            _ => Err(StoreError::InvalidData(
                "harness invocation state is malformed".to_owned(),
            )),
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed | Self::OutcomeUnknown
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessInvocationView {
    pub invocation_id: HarnessInvocationId,
    pub command_id: AgentCommandId,
    pub command_revision: u64,
    pub profile_id: HarnessProfileId,
    pub profile_digest: String,
    pub payload_digest: String,
    pub state: HarnessInvocationState,
    pub attempt: u64,
    pub prompt_accepted: bool,
    pub terminal: bool,
    pub terminal_request_digest: Option<String>,
    pub terminal_payload: Option<serde_json::Value>,
}

pub struct PrepareHarnessInvocation<'a> {
    pub invocation_id: &'a HarnessInvocationId,
    pub command_id: &'a AgentCommandId,
    pub command_revision: u64,
    pub profile_id: &'a HarnessProfileId,
    pub profile_digest: &'a str,
    pub payload_digest: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessInvocationEventRecord {
    pub invocation_id: HarnessInvocationId,
    pub sequence: u64,
    pub event_digest: String,
    pub event: HarnessInvocationEvent,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HarnessEventDigestInput<'a> {
    protocol_version: u16,
    invocation_id: &'a HarnessInvocationId,
    sequence: u64,
    event: &'a HarnessInvocationEvent,
}

impl HarnessInvocationEventRecord {
    pub fn new(
        invocation_id: HarnessInvocationId,
        sequence: u64,
        event: HarnessInvocationEvent,
    ) -> Result<Self, StoreError> {
        let event_digest = canonical_sha256(&HarnessEventDigestInput {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            invocation_id: &invocation_id,
            sequence,
            event: &event,
        })
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        Ok(Self {
            invocation_id,
            sequence,
            event_digest,
            event,
        })
    }

    pub fn validate_digest(&self) -> Result<(), StoreError> {
        let digest = canonical_sha256(&HarnessEventDigestInput {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            invocation_id: &self.invocation_id,
            sequence: self.sequence,
            event: &self.event,
        })
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let legacy_digest = canonical_sha256(&self.event)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        if self.event_digest == digest || self.event_digest == legacy_digest {
            Ok(())
        } else {
            Err(StoreError::InvalidData(
                "harness invocation event digest is malformed".to_owned(),
            ))
        }
    }

    pub fn is_compatible_replay_of(&self, incoming: &Self) -> Result<bool, StoreError> {
        self.validate_digest()?;
        incoming.validate_digest()?;
        if self.invocation_id != incoming.invocation_id || self.sequence != incoming.sequence {
            return Ok(false);
        }
        let stored_event = canonical_sha256(&self.event)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let incoming_event = canonical_sha256(&incoming.event)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        Ok(stored_event == incoming_event)
    }
}

pub(crate) fn harness_event_from_value(value: Value) -> Result<HarnessInvocationEvent, StoreError> {
    let object = value.as_object().ok_or_else(|| {
        StoreError::InvalidData("harness invocation event is malformed".to_owned())
    })?;
    let only = |keys: &[&str]| {
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
    };
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                StoreError::InvalidData("harness invocation event is malformed".to_owned())
            })
    };
    match object.get("type").and_then(Value::as_str) {
        Some("started") if only(&["type"]) => Ok(HarnessInvocationEvent::Started),
        Some("cancelled") if only(&["type"]) => Ok(HarnessInvocationEvent::Cancelled),
        Some("progress") if only(&["type", "message"]) => Ok(HarnessInvocationEvent::Progress {
            message: text("message")?,
        }),
        Some("assistant_message") if only(&["type", "text"]) => {
            Ok(HarnessInvocationEvent::AssistantMessage {
                text: text("text")?,
            })
        }
        Some("approval_requested") if only(&["type", "request_id", "title"]) => {
            Ok(HarnessInvocationEvent::ApprovalRequested {
                request_id: text("request_id")?,
                title: text("title")?,
            })
        }
        Some("approval_resolved") if only(&["type", "request_id", "decision"]) => {
            let decision = match text("decision")?.as_str() {
                "allow_once" => HarnessApprovalDecision::AllowOnce,
                "reject_once" => HarnessApprovalDecision::RejectOnce,
                _ => {
                    return Err(StoreError::InvalidData(
                        "harness invocation event is malformed".to_owned(),
                    ));
                }
            };
            Ok(HarnessInvocationEvent::ApprovalResolved {
                request_id: text("request_id")?,
                decision,
            })
        }
        _ => Err(StoreError::InvalidData(
            "harness invocation event is malformed".to_owned(),
        )),
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
    #[error("user task revision conflict")]
    TaskRevisionConflict,
    #[error("goal revision conflict")]
    GoalRevisionConflict,
    #[error("goal is already terminal")]
    GoalAlreadyTerminal,
    #[error("goal evidence does not cover the accepted criteria")]
    GoalEvidenceIncomplete,
    #[error("goal evidence was not found")]
    GoalEvidenceNotFound,
    #[error("goal evidence type does not match the criterion")]
    GoalEvidenceTypeMismatch,
    #[error("goal evidence source is not authorized")]
    GoalEvidenceUnauthorized,
    #[error("goal evidence is stale")]
    GoalEvidenceStale,
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
