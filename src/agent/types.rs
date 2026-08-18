use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dsl::ModelRole;
use crate::{
    ClosedSchema, CriterionAssertion, EvidenceId, GoalContract, IdempotencyKey, SessionId,
    UserTaskId,
};

use super::AgentRuntimeError;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AgentCommandId(uuid::Uuid);

impl AgentCommandId {
    pub const fn new(value: uuid::Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaimAgentCommand {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub command_id: AgentCommandId,
    pub expected_command_revision: u64,
    pub claimant: String,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteAgentCommand {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub command_id: AgentCommandId,
    pub expected_command_revision: u64,
    pub idempotency_key: IdempotencyKey,
    pub event: RuntimeEvent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailAgentCommand {
    pub session_id: SessionId,
    pub task_id: UserTaskId,
    pub command_id: AgentCommandId,
    pub expected_command_revision: u64,
    pub idempotency_key: IdempotencyKey,
    pub code: String,
}

pub(crate) fn principal_resumed_event_id(
    task_id: &UserTaskId,
    idempotency_key: &IdempotencyKey,
) -> String {
    let digest = Sha256::digest(idempotency_key.as_str().as_bytes());
    format!("principal-resumed:{}:{digest:x}", task_id.as_uuid())
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RuntimeCommandKey(String);

impl RuntimeCommandKey {
    pub fn new(value: impl Into<String>) -> Result<Self, AgentRuntimeError> {
        let value = value.into();
        let valid = (1..=128).contains(&value.len())
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.' | b'/' | b':')
            });
        valid
            .then_some(Self(value))
            .ok_or_else(|| AgentRuntimeError::InvalidTransition("command key is invalid".into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDisposition {
    Running,
    Returned,
    Blocked,
    Terminating,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeStateSnapshot {
    pub schema_version: u16,
    pub runtime_id: String,
    pub disposition: RuntimeDisposition,
    pub payload: serde_json::Value,
    pub ceiling_bytes: u32,
    pub canonical_byte_count: u32,
    pub snapshot_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCeilings {
    pub max_snapshot_bytes: usize,
    pub max_command_bytes: usize,
    pub max_commands_per_transition: usize,
    pub max_frames: usize,
    pub max_history_items_per_frame: usize,
}

impl Default for RuntimeCeilings {
    fn default() -> Self {
        Self {
            max_snapshot_bytes: 262_144,
            max_command_bytes: 65_536,
            max_commands_per_transition: 64,
            max_frames: 256,
            max_history_items_per_frame: 256,
        }
    }
}

impl RuntimeCeilings {
    pub fn validate(&self) -> Result<(), AgentRuntimeError> {
        let defaults = Self::default();
        let valid = (1..=defaults.max_snapshot_bytes).contains(&self.max_snapshot_bytes)
            && (1..=defaults.max_command_bytes).contains(&self.max_command_bytes)
            && (1..=defaults.max_commands_per_transition)
                .contains(&self.max_commands_per_transition)
            && (1..=defaults.max_frames).contains(&self.max_frames)
            && (1..=defaults.max_history_items_per_frame)
                .contains(&self.max_history_items_per_frame);
        valid
            .then_some(())
            .ok_or(AgentRuntimeError::CeilingExceeded("configured_ceilings"))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessStructuralLimits {
    pub model_turns_remaining: u64,
    pub tool_calls_per_response: u64,
    pub response_item_bytes: u64,
    pub active_execution_millis_remaining: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHistoryKind {
    HarnessInput,
    HarnessOutput,
    ToolResult,
    TaskReturn,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeHistoryItem {
    pub sequence: u64,
    pub kind: RuntimeHistoryKind,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessTool {
    pub qualified_name: String,
    pub descriptor_digest: String,
    pub descriptor: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalCriterionProjection {
    pub id: String,
    pub evidence_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalProjection {
    pub objective: String,
    pub success: Vec<GoalCriterionProjection>,
    pub constraints: Vec<String>,
}

impl GoalProjection {
    pub fn from_contract(contract: &GoalContract) -> Self {
        Self {
            objective: contract.objective.clone(),
            success: contract
                .success
                .iter()
                .map(|criterion| GoalCriterionProjection {
                    id: criterion.id.clone(),
                    evidence_type: criterion.evidence.clone(),
                })
                .collect(),
            constraints: contract.constraints.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvokeHarness {
    pub role: ModelRole,
    pub prompt: String,
    pub goal: GoalProjection,
    pub input: serde_json::Value,
    pub input_schema: ClosedSchema,
    pub result_schema: ClosedSchema,
    pub tools: Vec<HarnessTool>,
    pub projected_history: Vec<RuntimeHistoryItem>,
    pub limits: HarnessStructuralLimits,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestContext {
    pub evidence_type: String,
    pub subject: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitForEvent {
    pub event_type: String,
    pub correlation: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestApproval {
    pub question: String,
    pub answer_schema: ClosedSchema,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteGoal {
    pub assertions: Vec<CriterionAssertion>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockGoal {
    pub blocked_when_index: u16,
    pub reason_code: String,
    pub explanation: String,
    pub evidence_ids: Vec<EvidenceId>,
    pub unlock_conditions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailTask {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    rename_all = "snake_case",
    tag = "type",
    content = "payload",
    deny_unknown_fields
)]
#[allow(
    clippy::large_enum_variant,
    reason = "InvokeHarness is intentionally unboxed to preserve the frozen public and wire contract"
)]
pub enum KernelCommandPayload {
    RequestContext(RequestContext),
    WaitForEvent(WaitForEvent),
    InvokeHarness(InvokeHarness),
    RequestApproval(RequestApproval),
    CompleteGoal(CompleteGoal),
    BlockGoal(BlockGoal),
    FailTask(FailTask),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KernelCommand {
    pub schema_version: u16,
    pub key: RuntimeCommandKey,
    pub payload: KernelCommandPayload,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeTransition {
    pub idempotency_key: String,
    pub expected_task_revision: u64,
    pub expected_goal_revision: u64,
    pub snapshot: RuntimeStateSnapshot,
    pub commands: Vec<KernelCommand>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    rename_all = "snake_case",
    tag = "type",
    content = "payload",
    deny_unknown_fields
)]
pub enum RuntimeEvent {
    HarnessResult {
        event_id: String,
        command_key: RuntimeCommandKey,
        outcome: String,
        fields: serde_json::Value,
        evidence_id: EvidenceId,
        history: Vec<RuntimeHistoryItem>,
    },
    ContextResolved {
        event_id: String,
        command_key: RuntimeCommandKey,
        evidence_ids: Vec<EvidenceId>,
    },
    SemanticEvent {
        event_id: String,
        command_key: RuntimeCommandKey,
        event_type: String,
        data: serde_json::Value,
    },
    ApprovalResult {
        event_id: String,
        command_key: RuntimeCommandKey,
        answer: serde_json::Value,
    },
    CommandFailed {
        event_id: String,
        command_key: RuntimeCommandKey,
        code: String,
    },
    PrincipalResumed {
        event_id: String,
    },
}

impl RuntimeEvent {
    pub fn event_id(&self) -> &str {
        match self {
            Self::HarnessResult { event_id, .. }
            | Self::ContextResolved { event_id, .. }
            | Self::SemanticEvent { event_id, .. }
            | Self::ApprovalResult { event_id, .. }
            | Self::CommandFailed { event_id, .. }
            | Self::PrincipalResumed { event_id } => event_id,
        }
    }

    pub fn command_key(&self) -> Option<&RuntimeCommandKey> {
        match self {
            Self::HarnessResult { command_key, .. }
            | Self::ContextResolved { command_key, .. }
            | Self::SemanticEvent { command_key, .. }
            | Self::ApprovalResult { command_key, .. }
            | Self::CommandFailed { command_key, .. } => Some(command_key),
            Self::PrincipalResumed { .. } => None,
        }
    }
}
