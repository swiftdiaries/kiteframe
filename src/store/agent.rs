use serde_json::Value;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

use crate::dsl::{CompiledDefinition, CompiledToolBinding, DefinitionKind};
use crate::{
    AgentExecutionView, CatalogEpochId, CatalogTool, EvidenceId, GoalBlocker, GoalPhase,
    OperationalStatus, PolicyRevision, RuntimeCeilings, RuntimeDisposition, RuntimeStateSnapshot,
    SessionId, TaskLifecycle, UserTaskId,
};

use super::{
    AgentStartSeed, AgentTaskEvent, AgentTaskEventKind, PostgresStore, StoreError,
    postgres::user_task_from_row,
};

const SNAPSHOT_INTEGRITY_ERROR: &str = "stored snapshot integrity check failed";

#[allow(dead_code)] // Shared lock projection consumed by the later Slice 2 mutations.
pub(super) struct LockedAgentAggregate {
    pub(super) session_principal: String,
    pub(super) session_state: String,
    pub(super) session_policy_revision: PolicyRevision,
    pub(super) task_principal: String,
    pub(super) task_lifecycle: TaskLifecycle,
    pub(super) operational_status: OperationalStatus,
    pub(super) task_revision: u64,
    pub(super) definition_id: uuid::Uuid,
    pub(super) goal_phase: GoalPhase,
    pub(super) goal_revision: u64,
    pub(super) current_blocker_id: Option<uuid::Uuid>,
}

impl PostgresStore {
    #[must_use]
    pub const fn agent_runtime_ceilings(&self) -> &RuntimeCeilings {
        &self.limits.agent_runtime
    }

    pub async fn get_agent_execution(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
    ) -> Result<AgentExecutionView, StoreError> {
        let row = self.agent_row(&session_id, &task_id).await?;
        agent_execution_from_row(&row, self.agent_runtime_ceilings())
    }

    pub async fn load_agent_start_seed(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
    ) -> Result<AgentStartSeed, StoreError> {
        let row = self.agent_row(&session_id, &task_id).await?;
        Ok(AgentStartSeed {
            execution: agent_execution_from_row(&row, self.agent_runtime_ceilings())?,
            definition: definition_from_agent_row(&row)?,
        })
    }

    pub async fn agent_task_events_after(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<AgentTaskEvent>, StoreError> {
        let rows = sqlx::query(
            "SELECT e.sequence, e.kind, e.metadata, e.task_revision, e.goal_revision, \
                    e.created_at \
             FROM agent_task_events e \
             JOIN user_tasks t ON t.id = e.task_id \
             WHERE e.task_id = $1 AND t.session_id = $2 AND e.sequence > $3 \
             ORDER BY e.sequence LIMIT $4",
        )
        .bind(task_id.as_uuid())
        .bind(session_id.as_uuid())
        .bind(i64::try_from(after).unwrap_or(i64::MAX))
        .bind(i64::from(limit.clamp(1, 256)))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(agent_task_event_from_row).collect()
    }

    async fn agent_row(
        &self,
        session_id: &SessionId,
        task_id: &UserTaskId,
    ) -> Result<PgRow, StoreError> {
        sqlx::query(
            "SELECT t.id AS task_id, t.session_id, t.principal, t.request_digest, \
                    t.target, t.preferences, t.state AS task_state, t.operational_status, \
                    t.revision, t.created_at, \
                    d.id AS definition_id, d.definition_kind, d.name AS definition_name, \
                    d.version AS definition_version, d.definition_hash, d.compiled_definition, \
                    g.task_id AS goal_task_id, g.contract_version, g.objective, \
                    g.success_criteria, g.constraints, g.blocked_when, g.contract_digest, \
                    gs.phase AS goal_phase, gs.revision AS goal_revision, \
                    gs.current_blocker_id, \
                    b.blocked_when_index, b.reason_code AS blocker_reason_code, \
                    b.explanation AS blocker_explanation, b.evidence_ids AS blocker_evidence_ids, \
                    b.unlock_conditions AS blocker_unlock_conditions, \
                    b.goal_revision AS blocker_goal_revision, \
                    rs.task_revision AS runtime_task_revision, \
                    rs.schema_version AS runtime_schema_version, rs.runtime_id, \
                    rs.disposition AS runtime_disposition, rs.payload AS runtime_payload, \
                    rs.ceiling_bytes AS runtime_ceiling_bytes, \
                    rs.canonical_bytes AS runtime_canonical_bytes, \
                    rs.snapshot_digest AS runtime_snapshot_digest \
             FROM user_tasks t \
             JOIN workflow_definitions d ON d.id = t.definition_id \
             LEFT JOIN goal_contracts g ON g.task_id = t.id \
             LEFT JOIN goal_states gs ON gs.task_id = t.id \
             LEFT JOIN goal_blockers b \
               ON b.task_id = gs.task_id AND b.id = gs.current_blocker_id \
             LEFT JOIN agent_runtime_snapshots rs ON rs.task_id = t.id \
             WHERE t.session_id = $1 AND t.id = $2",
        )
        .bind(session_id.as_uuid())
        .bind(task_id.as_uuid())
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::UserTaskNotFound)
    }
}

#[allow(dead_code)] // Establishes the mandatory prefix before later mutations are added.
pub(super) async fn lock_agent_aggregate(
    tx: &mut Transaction<'_, Postgres>,
    session_id: &SessionId,
    task_id: &UserTaskId,
) -> Result<LockedAgentAggregate, StoreError> {
    let session = sqlx::query(
        "SELECT principal, state, current_policy_revision \
         FROM sessions WHERE id = $1 FOR UPDATE",
    )
    .bind(session_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::SessionNotFound)?;
    let task = sqlx::query(
        "SELECT principal, state, operational_status, revision, definition_id \
         FROM user_tasks WHERE id = $1 AND session_id = $2 FOR UPDATE",
    )
    .bind(task_id.as_uuid())
    .bind(session_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::UserTaskNotFound)?;
    let goal = sqlx::query(
        "SELECT phase, revision, current_blocker_id FROM goal_states \
         WHERE task_id = $1 FOR UPDATE",
    )
    .bind(task_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::InvalidData("goal state is absent".to_owned()))?;

    let aggregate = LockedAgentAggregate::from_rows(&session, &task, &goal)?;
    if aggregate.session_principal != aggregate.task_principal {
        return Err(StoreError::PrincipalMismatch);
    }
    if aggregate.session_state != "active" {
        return Err(StoreError::SessionClosed);
    }
    Ok(aggregate)
}

impl LockedAgentAggregate {
    #[allow(dead_code)]
    fn from_rows(session: &PgRow, task: &PgRow, goal: &PgRow) -> Result<Self, StoreError> {
        Ok(Self {
            session_principal: session.get("principal"),
            session_state: session.get("state"),
            session_policy_revision: PolicyRevision::new(nonnegative(
                session.get("current_policy_revision"),
                "session policy revision is malformed",
            )?),
            task_principal: task.get("principal"),
            task_lifecycle: task_lifecycle(&task.get::<String, _>("state"))?,
            operational_status: operational_status(&task.get::<String, _>("operational_status"))?,
            task_revision: nonnegative(task.get("revision"), "user task revision is malformed")?,
            definition_id: task.get("definition_id"),
            goal_phase: goal_phase(&goal.get::<String, _>("phase"))?,
            goal_revision: nonnegative(goal.get("revision"), "goal revision is malformed")?,
            current_blocker_id: goal.get("current_blocker_id"),
        })
    }
}

pub(super) fn agent_execution_from_row(
    row: &PgRow,
    ceilings: &RuntimeCeilings,
) -> Result<AgentExecutionView, StoreError> {
    let task = user_task_from_row(row)?;
    let lifecycle = task_lifecycle(&row.get::<String, _>("task_state"))?;
    let operational_status = operational_status(&row.get::<String, _>("operational_status"))?;
    let task_revision = nonnegative(row.get("revision"), "user task revision is malformed")?;
    if task.revision != task_revision {
        return Err(StoreError::InvalidData(
            "user task revision is malformed".to_owned(),
        ));
    }
    let goal_phase = row
        .get::<Option<String>, _>("goal_phase")
        .ok_or_else(|| StoreError::InvalidData("goal state is absent".to_owned()))
        .and_then(|value| goal_phase(&value))?;
    let goal_revision = row
        .get::<Option<i64>, _>("goal_revision")
        .ok_or_else(|| StoreError::InvalidData("goal state is absent".to_owned()))
        .and_then(|value| nonnegative(value, "goal revision is malformed"))?;
    let blocker = blocker_from_row(row)?;
    let runtime = runtime_from_row(row, task_revision, ceilings)?;

    Ok(AgentExecutionView {
        task,
        lifecycle,
        operational_status,
        task_revision,
        goal_phase,
        goal_revision,
        blocker,
        runtime,
    })
}

pub(super) fn definition_from_agent_row(row: &PgRow) -> Result<CompiledDefinition, StoreError> {
    if definition_kind(&row.get::<String, _>("definition_kind"))? != DefinitionKind::Agent {
        return Err(StoreError::AgentDefinitionRequired);
    }
    let compiled_value: Value = row.get("compiled_definition");
    let tool_bindings = compiled_tool_bindings(&compiled_value)?;
    let mut definition: CompiledDefinition = serde_json::from_value(compiled_value)
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    definition.tool_bindings = tool_bindings;
    if definition.dsl_version != 2
        || definition.definition_kind != DefinitionKind::Agent
        || definition.definition_hash != row.get::<String, _>("definition_hash")
    {
        return Err(StoreError::InvalidData(
            "stored agent definition is malformed".to_owned(),
        ));
    }
    Ok(definition)
}

fn compiled_tool_bindings(value: &Value) -> Result<Vec<CompiledToolBinding>, StoreError> {
    #[derive(serde::Deserialize)]
    struct StoredBinding {
        qualified_name: String,
        catalog_epoch_id: CatalogEpochId,
        descriptor_digest: String,
        descriptor: Value,
    }

    let Some(bindings) = value.get("tool_bindings") else {
        return Ok(Vec::new());
    };
    let bindings = bindings.as_array().ok_or_else(|| {
        StoreError::InvalidData("stored agent tool bindings are malformed".to_owned())
    })?;
    bindings
        .iter()
        .map(|binding| {
            let stored: StoredBinding = serde_json::from_value(binding.clone())
                .map_err(|error| StoreError::InvalidData(error.to_string()))?;
            let descriptor = CatalogTool::from_normalized(&stored.descriptor)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?;
            if descriptor.qualified_name() != stored.qualified_name
                || descriptor.descriptor_digest() != stored.descriptor_digest
            {
                return Err(StoreError::InvalidData(
                    "stored agent tool binding is malformed".to_owned(),
                ));
            }
            Ok(CompiledToolBinding {
                qualified_name: stored.qualified_name,
                catalog_epoch_id: stored.catalog_epoch_id,
                descriptor_digest: stored.descriptor_digest,
                descriptor,
            })
        })
        .collect()
}

pub(super) async fn append_agent_task_event(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    kind: AgentTaskEventKind,
    metadata: Value,
    task_revision: u64,
    goal_revision: u64,
) -> Result<(), StoreError> {
    let sequence: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM agent_task_events WHERE task_id = $1",
    )
    .bind(task_id.as_uuid())
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO agent_task_events \
         (task_id, sequence, kind, metadata, task_revision, goal_revision) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(task_id.as_uuid())
    .bind(sequence)
    .bind(kind.as_str())
    .bind(metadata)
    .bind(to_i64(task_revision, "user task revision is malformed")?)
    .bind(to_i64(goal_revision, "goal revision is malformed")?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn blocker_from_row(row: &PgRow) -> Result<Option<GoalBlocker>, StoreError> {
    let Some(blocker_id) = row.get::<Option<uuid::Uuid>, _>("current_blocker_id") else {
        return Ok(None);
    };
    if row.get::<Option<i32>, _>("blocked_when_index").is_none() {
        return Err(StoreError::InvalidData(
            "current goal blocker is absent".to_owned(),
        ));
    }
    let blocked_when_index = u16::try_from(row.get::<i32, _>("blocked_when_index"))
        .map_err(|_| StoreError::InvalidData("goal blocker index is malformed".to_owned()))?;
    let evidence_ids: Vec<EvidenceId> = serde_json::from_value(row.get("blocker_evidence_ids"))
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    let unlock_conditions: Vec<String> =
        serde_json::from_value(row.get("blocker_unlock_conditions"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    let blocker = GoalBlocker {
        blocked_when_index,
        reason_code: row.get("blocker_reason_code"),
        explanation: row.get("blocker_explanation"),
        evidence_ids,
        unlock_conditions,
        goal_revision: nonnegative(
            row.get("blocker_goal_revision"),
            "goal blocker revision is malformed",
        )?,
    };
    let _ = blocker_id;
    Ok(Some(blocker))
}

fn runtime_from_row(
    row: &PgRow,
    task_revision: u64,
    ceilings: &RuntimeCeilings,
) -> Result<Option<RuntimeStateSnapshot>, StoreError> {
    let Some(schema_version) = row.get::<Option<i32>, _>("runtime_schema_version") else {
        return Ok(None);
    };
    let runtime_task_revision = row
        .get::<Option<i64>, _>("runtime_task_revision")
        .ok_or_else(snapshot_integrity_error)
        .and_then(nonnegative_runtime)?;
    if runtime_task_revision != task_revision {
        return Err(snapshot_integrity_error());
    }
    let snapshot = RuntimeStateSnapshot {
        schema_version: u16::try_from(schema_version).map_err(|_| snapshot_integrity_error())?,
        runtime_id: row.get("runtime_id"),
        disposition: runtime_disposition(&row.get::<String, _>("runtime_disposition"))?,
        payload: row.get("runtime_payload"),
        ceiling_bytes: u32::try_from(row.get::<i32, _>("runtime_ceiling_bytes"))
            .map_err(|_| snapshot_integrity_error())?,
        canonical_byte_count: u32::try_from(row.get::<i32, _>("runtime_canonical_bytes"))
            .map_err(|_| snapshot_integrity_error())?,
        snapshot_digest: row.get("runtime_snapshot_digest"),
    };
    let bytes = snapshot
        .canonical_bytes(ceilings)
        .map_err(|_| snapshot_integrity_error())?;
    let digest = snapshot
        .digest(ceilings)
        .map_err(|_| snapshot_integrity_error())?;
    if bytes.len() != snapshot.canonical_byte_count as usize
        || digest.as_str() != snapshot.snapshot_digest.as_str()
    {
        return Err(snapshot_integrity_error());
    }
    Ok(Some(snapshot))
}

fn agent_task_event_from_row(row: &PgRow) -> Result<AgentTaskEvent, StoreError> {
    Ok(AgentTaskEvent {
        sequence: positive(row.get("sequence"), "agent event sequence is malformed")?,
        kind: AgentTaskEventKind::parse(&row.get::<String, _>("kind"))?,
        metadata: row.get("metadata"),
        task_revision: nonnegative(row.get("task_revision"), "user task revision is malformed")?,
        goal_revision: nonnegative(row.get("goal_revision"), "goal revision is malformed")?,
        created_at: row.get("created_at"),
    })
}

fn goal_phase(value: &str) -> Result<GoalPhase, StoreError> {
    match value {
        "active" => Ok(GoalPhase::Active),
        "paused" => Ok(GoalPhase::Paused),
        "blocked" => Ok(GoalPhase::Blocked),
        "complete" => Ok(GoalPhase::Complete),
        _ => Err(StoreError::InvalidData(
            "goal phase is malformed".to_owned(),
        )),
    }
}

fn task_lifecycle(value: &str) -> Result<TaskLifecycle, StoreError> {
    match value {
        "accepted" => Ok(TaskLifecycle::Accepted),
        "running" => Ok(TaskLifecycle::Running),
        "succeeded" => Ok(TaskLifecycle::Succeeded),
        "failed" => Ok(TaskLifecycle::Failed),
        "cancelled" => Ok(TaskLifecycle::Cancelled),
        _ => Err(StoreError::InvalidData(
            "task lifecycle is malformed".to_owned(),
        )),
    }
}

fn operational_status(value: &str) -> Result<OperationalStatus, StoreError> {
    match value {
        "not_started" => Ok(OperationalStatus::NotStarted),
        "runnable" => Ok(OperationalStatus::Runnable),
        "waiting_for_context" => Ok(OperationalStatus::WaitingForContext),
        "waiting_for_event" => Ok(OperationalStatus::WaitingForEvent),
        "waiting_for_harness" => Ok(OperationalStatus::WaitingForHarness),
        "waiting_for_approval" => Ok(OperationalStatus::WaitingForApproval),
        "paused" => Ok(OperationalStatus::Paused),
        "blocked" => Ok(OperationalStatus::Blocked),
        "finished" => Ok(OperationalStatus::Finished),
        _ => Err(StoreError::InvalidData(
            "operational status is malformed".to_owned(),
        )),
    }
}

fn runtime_disposition(value: &str) -> Result<RuntimeDisposition, StoreError> {
    match value {
        "running" => Ok(RuntimeDisposition::Running),
        "returned" => Ok(RuntimeDisposition::Returned),
        "blocked" => Ok(RuntimeDisposition::Blocked),
        "terminating" => Ok(RuntimeDisposition::Terminating),
        _ => Err(snapshot_integrity_error()),
    }
}

fn definition_kind(value: &str) -> Result<DefinitionKind, StoreError> {
    match value {
        "workflow" => Ok(DefinitionKind::Workflow),
        "agent" => Ok(DefinitionKind::Agent),
        _ => Err(StoreError::InvalidData(
            "definition kind is malformed".to_owned(),
        )),
    }
}

fn nonnegative(value: i64, message: &str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidData(message.to_owned()))
}

fn positive(value: i64, message: &str) -> Result<u64, StoreError> {
    let value = nonnegative(value, message)?;
    (value > 0)
        .then_some(value)
        .ok_or_else(|| StoreError::InvalidData(message.to_owned()))
}

fn nonnegative_runtime(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| snapshot_integrity_error())
}

fn to_i64(value: u64, message: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(message.to_owned()))
}

fn snapshot_integrity_error() -> StoreError {
    StoreError::RuntimeStateInvalid(SNAPSHOT_INTEGRITY_ERROR.to_owned())
}
