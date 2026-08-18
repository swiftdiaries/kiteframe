use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use std::collections::BTreeSet;

use crate::dsl::{CompiledDefinition, CompiledToolBinding, DefinitionKind};
use crate::{
    AgentExecutionView, AgentStartCommit, CatalogEpochId, CatalogTool, EvidenceId, GoalBlocker,
    GoalPhase, KernelCommand, KernelCommandPayload, OperationalStatus, PolicyRevision, PrincipalId,
    RuntimeCeilings, RuntimeDisposition, RuntimeStateSnapshot, SessionId, StartAgent,
    TaskLifecycle, UserTaskId, canonical_sha256,
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
    pub(super) definition_hash: String,
    pub(super) goal_phase: GoalPhase,
    pub(super) goal_revision: u64,
    pub(super) current_blocker_id: Option<uuid::Uuid>,
}

#[allow(dead_code)]
struct AgentActionReplay {
    request_digest: String,
    result_task_revision: u64,
    result_goal_revision: u64,
}

impl AgentActionReplay {
    fn from_row(row: &PgRow) -> Result<Self, StoreError> {
        Ok(Self {
            request_digest: row.get("request_digest"),
            result_task_revision: nonnegative(
                row.get("result_task_revision"),
                "agent action task revision is malformed",
            )?,
            result_goal_revision: nonnegative(
                row.get("result_goal_revision"),
                "agent action goal revision is malformed",
            )?,
        })
    }
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

    pub async fn replay_agent_start(
        &self,
        principal: &PrincipalId,
        request: &StartAgent,
    ) -> Result<Option<AgentExecutionView>, StoreError> {
        let request_digest = start_request_digest(request)?;
        let session = sqlx::query("SELECT principal, state FROM sessions WHERE id = $1")
            .bind(request.session_id.as_uuid())
            .fetch_optional(self.pool())
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        if session.get::<String, _>("principal") != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if session.get::<String, _>("state") != "active" {
            return Err(StoreError::SessionClosed);
        }
        let task_principal: String = sqlx::query_scalar(
            "SELECT principal FROM user_tasks WHERE id = $1 AND session_id = $2",
        )
        .bind(request.task_id.as_uuid())
        .bind(request.session_id.as_uuid())
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::UserTaskNotFound)?;
        if task_principal != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        let replay = sqlx::query(
            "SELECT request_digest, result_task_revision, result_goal_revision \
             FROM agent_task_actions \
             WHERE task_id = $1 AND action_kind = 'start' AND idempotency_key = $2",
        )
        .bind(request.task_id.as_uuid())
        .bind(request.idempotency_key.as_str())
        .fetch_optional(self.pool())
        .await?
        .map(|row| AgentActionReplay::from_row(&row))
        .transpose()?;
        let Some(replay) = replay else {
            return Ok(None);
        };
        if replay.request_digest != request_digest {
            return Err(StoreError::IdempotencyConflict);
        }
        let execution = self
            .get_agent_execution(request.session_id.clone(), request.task_id.clone())
            .await?;
        Ok(Some(execution))
    }

    pub async fn commit_agent_start(
        &self,
        commit: AgentStartCommit<'_>,
    ) -> Result<AgentExecutionView, StoreError> {
        let deployment_ceilings = self.agent_runtime_ceilings();
        if commit.ceilings != deployment_ceilings {
            return Err(StoreError::AgentTransitionInvalid(
                "runtime ceilings do not match deployment configuration".to_owned(),
            ));
        }
        validate_start_commit(&commit, deployment_ceilings)?;
        let snapshot_bytes = commit
            .transition
            .snapshot
            .canonical_bytes(deployment_ceilings)
            .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
        let snapshot_digest = commit
            .transition
            .snapshot
            .digest(deployment_ceilings)
            .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
        let command_digests = commit
            .transition
            .commands
            .iter()
            .map(canonical_sha256)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        let transition_digest = canonical_sha256(&json!({
            "expectedTaskRevision": commit.transition.expected_task_revision,
            "expectedGoalRevision": commit.transition.expected_goal_revision,
            "snapshot": &commit.transition.snapshot,
            "commands": &commit.transition.commands,
        }))
        .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        let request_digest = start_request_digest(commit.request)?;
        let operational_status = operational_status_for_commands(
            commit.transition.snapshot.disposition,
            &commit.transition.commands,
        )?;
        if !commit.policy.allowed {
            return Err(StoreError::AgentTransitionInvalid(
                "current policy denied agent start".to_owned(),
            ));
        }
        if !valid_reason_code(&commit.policy.reason_code) {
            return Err(StoreError::AgentTransitionInvalid(
                "policy reason code is invalid".to_owned(),
            ));
        }

        let mut tx = self.pool().begin().await?;
        let aggregate =
            lock_agent_aggregate(&mut tx, &commit.request.session_id, &commit.request.task_id)
                .await?;
        if aggregate.task_principal != commit.principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if aggregate.session_policy_revision != commit.policy.policy_revision {
            return Err(StoreError::PolicyRevisionConflict);
        }
        if let Some(replay) = load_action_replay(
            &mut tx,
            &commit.request.task_id,
            "start",
            commit.request.idempotency_key.as_str(),
        )
        .await?
        {
            if replay.request_digest != request_digest {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            let execution = self
                .get_agent_execution(
                    commit.request.session_id.clone(),
                    commit.request.task_id.clone(),
                )
                .await?;
            return Ok(execution);
        }
        let has_snapshot: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM agent_runtime_snapshots WHERE task_id = $1)",
        )
        .bind(commit.request.task_id.as_uuid())
        .fetch_one(&mut *tx)
        .await?;
        if aggregate.task_lifecycle != TaskLifecycle::Accepted
            || aggregate.operational_status != OperationalStatus::NotStarted
            || aggregate.goal_phase != GoalPhase::Active
            || aggregate.current_blocker_id.is_some()
            || has_snapshot
        {
            return Err(StoreError::AgentTaskAlreadyTerminal);
        }
        if commit.request.expected_task_revision != aggregate.task_revision
            || commit.request.expected_goal_revision != aggregate.goal_revision
        {
            return Err(StoreError::RevisionConflict);
        }
        if aggregate.task_revision != 0
            || aggregate.goal_revision != 0
            || commit.transition.expected_task_revision != 0
            || commit.transition.expected_goal_revision != 0
        {
            return Err(StoreError::AgentTransitionInvalid(
                "agent start requires initial task and goal revisions".to_owned(),
            ));
        }
        if aggregate.definition_id != commit.frozen_definition_id
            || aggregate.definition_hash != commit.frozen_definition_hash
        {
            return Err(StoreError::AgentTransitionInvalid(
                "frozen agent definition changed before start".to_owned(),
            ));
        }

        let task_revision = 1_u64;
        let goal_revision = 0_u64;
        let transition_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO agent_transitions \
             (id, task_id, idempotency_key, request_digest, expected_task_revision, \
              expected_goal_revision, committed_task_revision, committed_goal_revision, \
              snapshot_digest, command_digests) \
             VALUES ($1, $2, $3, $4, 0, 0, 1, 0, $5, $6)",
        )
        .bind(transition_id)
        .bind(commit.request.task_id.as_uuid())
        .bind(&commit.transition.idempotency_key)
        .bind(transition_digest)
        .bind(&snapshot_digest)
        .bind(serde_json::to_value(&command_digests).map_err(invalid_data)?)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO agent_policy_decisions \
             (id, task_id, transition_id, action_kind, allowed, policy_revision, reason_code) \
             VALUES ($1, $2, $3, 'start', TRUE, $4, $5)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(commit.request.task_id.as_uuid())
        .bind(transition_id)
        .bind(to_i64(
            commit.policy.policy_revision.get(),
            "policy revision is malformed",
        )?)
        .bind(&commit.policy.reason_code)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO agent_runtime_snapshots \
             (task_id, task_revision, schema_version, runtime_id, disposition, payload, \
              ceiling_bytes, canonical_bytes, snapshot_digest) \
             VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(commit.request.task_id.as_uuid())
        .bind(i32::from(commit.transition.snapshot.schema_version))
        .bind(&commit.transition.snapshot.runtime_id)
        .bind(runtime_disposition_name(
            commit.transition.snapshot.disposition,
        ))
        .bind(&commit.transition.snapshot.payload)
        .bind(
            i32::try_from(commit.transition.snapshot.ceiling_bytes).map_err(|_| {
                StoreError::RuntimeStateInvalid("snapshot ceiling is malformed".to_owned())
            })?,
        )
        .bind(i32::try_from(snapshot_bytes.len()).map_err(|_| {
            StoreError::RuntimeStateInvalid("snapshot byte count is malformed".to_owned())
        })?)
        .bind(&snapshot_digest)
        .execute(&mut *tx)
        .await?;
        for (ordinal, command) in commit.transition.commands.iter().enumerate() {
            let payload = command_payload(&command.payload)?;
            let payload_digest = canonical_sha256(&payload)
                .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
            sqlx::query(
                "INSERT INTO agent_commands \
                 (id, task_id, transition_id, ordinal, schema_version, runtime_key, \
                  command_kind, payload, payload_digest) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(uuid::Uuid::now_v7())
            .bind(commit.request.task_id.as_uuid())
            .bind(transition_id)
            .bind(i32::try_from(ordinal).map_err(|_| {
                StoreError::AgentTransitionInvalid("command ordinal is invalid".to_owned())
            })?)
            .bind(i32::from(command.schema_version))
            .bind(command.key.as_str())
            .bind(command_kind(&command.payload))
            .bind(payload)
            .bind(payload_digest)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE user_tasks SET state = 'running', operational_status = $1, revision = 1, \
             started_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
        )
        .bind(operational_status_name(operational_status))
        .bind(commit.request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        append_agent_task_event(
            &mut tx,
            &commit.request.task_id,
            AgentTaskEventKind::AgentStarted,
            json!({
                "definition_id": commit.frozen_definition_id,
                "definition_hash": commit.frozen_definition_hash,
                "policy_revision": commit.policy.policy_revision.get(),
                "reason_code": commit.policy.reason_code,
                "snapshot_digest": snapshot_digest,
                "command_count": command_digests.len(),
            }),
            task_revision,
            goal_revision,
        )
        .await?;
        sqlx::query(
            "INSERT INTO agent_task_actions \
             (id, task_id, action_kind, idempotency_key, request_digest, \
              result_task_revision, result_goal_revision) \
             VALUES ($1, $2, 'start', $3, $4, 1, 0)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(commit.request.task_id.as_uuid())
        .bind(commit.request.idempotency_key.as_str())
        .bind(request_digest)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.get_agent_execution(
            commit.request.session_id.clone(),
            commit.request.task_id.clone(),
        )
        .await
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

async fn load_action_replay(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    action_kind: &str,
    idempotency_key: &str,
) -> Result<Option<AgentActionReplay>, StoreError> {
    sqlx::query(
        "SELECT request_digest, result_task_revision, result_goal_revision \
         FROM agent_task_actions \
         WHERE task_id = $1 AND action_kind = $2 AND idempotency_key = $3",
    )
    .bind(task_id.as_uuid())
    .bind(action_kind)
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await?
    .map(|row| AgentActionReplay::from_row(&row))
    .transpose()
}

fn start_request_digest(request: &StartAgent) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "sessionId": &request.session_id,
        "taskId": &request.task_id,
        "expectedTaskRevision": request.expected_task_revision,
        "expectedGoalRevision": request.expected_goal_revision,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn operational_status_for_commands(
    disposition: RuntimeDisposition,
    commands: &[KernelCommand],
) -> Result<OperationalStatus, StoreError> {
    if disposition == RuntimeDisposition::Returned && commands.is_empty() {
        return Ok(OperationalStatus::Finished);
    }
    if disposition != RuntimeDisposition::Running || commands.is_empty() {
        return Err(StoreError::AgentTransitionInvalid(
            "agent start must return or emit external commands".to_owned(),
        ));
    }
    let first = external_command_status(&commands[0].payload)?;
    for command in &commands[1..] {
        if external_command_status(&command.payload)? != first {
            return Err(StoreError::AgentTransitionInvalid(
                "agent start emitted mixed external command kinds".to_owned(),
            ));
        }
    }
    Ok(first)
}

fn external_command_status(
    payload: &KernelCommandPayload,
) -> Result<OperationalStatus, StoreError> {
    match payload {
        KernelCommandPayload::InvokeHarness(_) => Ok(OperationalStatus::WaitingForHarness),
        KernelCommandPayload::RequestContext(_) => Ok(OperationalStatus::WaitingForContext),
        KernelCommandPayload::WaitForEvent(_) => Ok(OperationalStatus::WaitingForEvent),
        KernelCommandPayload::RequestApproval(_) => Ok(OperationalStatus::WaitingForApproval),
        KernelCommandPayload::CompleteGoal(_)
        | KernelCommandPayload::BlockGoal(_)
        | KernelCommandPayload::FailTask(_) => Err(StoreError::AgentTransitionInvalid(
            "agent start cannot emit an internal authority command".to_owned(),
        )),
    }
}

fn validate_start_commit(
    commit: &AgentStartCommit<'_>,
    deployment_ceilings: &RuntimeCeilings,
) -> Result<(), StoreError> {
    deployment_ceilings
        .validate()
        .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    if !(1..=128).contains(&commit.transition.idempotency_key.len()) {
        return Err(StoreError::AgentTransitionInvalid(
            "runtime transition idempotency key is invalid".to_owned(),
        ));
    }
    if commit.transition.commands.len() > deployment_ceilings.max_commands_per_transition {
        return Err(StoreError::AgentTransitionInvalid(
            "runtime command ceiling exceeded".to_owned(),
        ));
    }
    let mut keys = BTreeSet::new();
    for command in &commit.transition.commands {
        if command.schema_version != 1
            || crate::RuntimeCommandKey::new(command.key.as_str()).is_err()
            || !keys.insert(command.key.as_str())
        {
            return Err(StoreError::AgentTransitionInvalid(
                "runtime command envelope is invalid".to_owned(),
            ));
        }
        let bytes = crate::canonical_json(command)
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        if bytes.len() > deployment_ceilings.max_command_bytes {
            return Err(StoreError::AgentTransitionInvalid(
                "runtime command ceiling exceeded".to_owned(),
            ));
        }
    }
    operational_status_for_commands(
        commit.transition.snapshot.disposition,
        &commit.transition.commands,
    )?;
    Ok(())
}

fn command_kind(payload: &KernelCommandPayload) -> &'static str {
    match payload {
        KernelCommandPayload::RequestContext(_) => "request_context",
        KernelCommandPayload::WaitForEvent(_) => "wait_for_event",
        KernelCommandPayload::InvokeHarness(_) => "invoke_harness",
        KernelCommandPayload::RequestApproval(_) => "request_approval",
        KernelCommandPayload::CompleteGoal(_) => "complete_goal",
        KernelCommandPayload::BlockGoal(_) => "block_goal",
        KernelCommandPayload::FailTask(_) => "fail_task",
    }
}

fn command_payload(payload: &KernelCommandPayload) -> Result<Value, StoreError> {
    match payload {
        KernelCommandPayload::RequestContext(value) => serde_json::to_value(value),
        KernelCommandPayload::WaitForEvent(value) => serde_json::to_value(value),
        KernelCommandPayload::InvokeHarness(value) => serde_json::to_value(value),
        KernelCommandPayload::RequestApproval(value) => serde_json::to_value(value),
        KernelCommandPayload::CompleteGoal(value) => serde_json::to_value(value),
        KernelCommandPayload::BlockGoal(value) => serde_json::to_value(value),
        KernelCommandPayload::FailTask(value) => serde_json::to_value(value),
    }
    .map_err(invalid_data)
}

const fn runtime_disposition_name(disposition: RuntimeDisposition) -> &'static str {
    match disposition {
        RuntimeDisposition::Running => "running",
        RuntimeDisposition::Returned => "returned",
        RuntimeDisposition::Blocked => "blocked",
        RuntimeDisposition::Terminating => "terminating",
    }
}

const fn operational_status_name(status: OperationalStatus) -> &'static str {
    match status {
        OperationalStatus::NotStarted => "not_started",
        OperationalStatus::Runnable => "runnable",
        OperationalStatus::WaitingForContext => "waiting_for_context",
        OperationalStatus::WaitingForEvent => "waiting_for_event",
        OperationalStatus::WaitingForHarness => "waiting_for_harness",
        OperationalStatus::WaitingForApproval => "waiting_for_approval",
        OperationalStatus::Paused => "paused",
        OperationalStatus::Blocked => "blocked",
        OperationalStatus::Finished => "finished",
    }
}

fn valid_reason_code(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(first) if first.is_ascii_lowercase())
        && value.len() <= 64
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn invalid_data(error: serde_json::Error) -> StoreError {
    StoreError::InvalidData(error.to_string())
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
    let definition =
        sqlx::query("SELECT definition_hash FROM workflow_definitions WHERE id = $1 FOR UPDATE")
            .bind(task.get::<uuid::Uuid, _>("definition_id"))
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(StoreError::DefinitionNotFound)?;

    let aggregate = LockedAgentAggregate::from_rows(&session, &task, &goal, &definition)?;
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
    fn from_rows(
        session: &PgRow,
        task: &PgRow,
        goal: &PgRow,
        definition: &PgRow,
    ) -> Result<Self, StoreError> {
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
            definition_hash: definition.get("definition_hash"),
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
