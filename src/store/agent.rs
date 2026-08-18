use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::agent::principal_resumed_event_id;
use crate::dsl::{CompiledDefinition, CompiledToolBinding, DefinitionKind};
use crate::{
    AgentCommandId, AgentExecutionView, AgentStartCommit, BlockGoal, CancelUserTask,
    CatalogEpochId, CatalogTool, ClaimAgentCommand, CompleteAgentCommand, CompleteGoal, EvidenceId,
    EvidenceRecord, EvidenceSourceIdentity, FailAgentCommand, GoalBlocker, GoalContract, GoalPhase,
    KernelCommand, KernelCommandPayload, KiteAgentRuntime, KiteFrameState, KiteRuntimeState,
    OperationalStatus, PauseUserTask, PolicyRevision, PrincipalId, ResumeUserTask, RuntimeCeilings,
    RuntimeCommandKey, RuntimeDisposition, RuntimeEvent, RuntimeStateSnapshot, RuntimeTransition,
    SessionId, StartAgent, TaskLifecycle, UserTaskId, canonical_sha256, goal_edge_allowed,
    validate_transition,
};

use super::{
    AgentCommandContext, AgentCommandState, AgentCommandView, AgentRecoveryReport, AgentStartSeed,
    AgentTaskEvent, AgentTaskEventKind, PostgresStore, StoreError, postgres::user_task_from_row,
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

struct LockedAgentCommand {
    view: AgentCommandView,
    claim_idempotency_key: Option<String>,
    claim_request_digest: Option<String>,
    completion_idempotency_key: Option<String>,
    completion_request_digest: Option<String>,
}

struct AuthorizedEvidence {
    evidence_type: String,
    source_authorized: bool,
    digest_current: bool,
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

    pub(crate) async fn authenticate_agent_task_owner(
        &self,
        principal: &PrincipalId,
        session_id: &SessionId,
        task_id: &UserTaskId,
    ) -> Result<(), StoreError> {
        authenticate_agent_owner(self, principal, session_id, task_id).await
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

    pub async fn dispatchable_agent_commands(
        &self,
        limit: i64,
    ) -> Result<Vec<AgentCommandView>, StoreError> {
        let rows = sqlx::query(
            "SELECT c.id, c.task_id, c.runtime_key, c.ordinal, c.schema_version, \
                    c.command_kind, c.payload, c.payload_digest, c.state, c.revision, \
                    c.claimant, c.claim_idempotency_key, c.claim_request_digest, \
                    c.completion_idempotency_key, c.completion_request_digest \
             FROM agent_commands c \
             JOIN user_tasks t ON t.id = c.task_id \
             JOIN sessions s ON s.id = t.session_id \
             JOIN goal_states g ON g.task_id = t.id \
             WHERE c.state IN ('pending', 'claimed') AND t.state = 'running' \
               AND t.operational_status NOT IN ('paused', 'blocked', 'finished') \
               AND g.phase = 'active' AND s.state = 'active' \
             ORDER BY c.created_at, c.id LIMIT $1",
        )
        .bind(limit.clamp(1, 256))
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(locked_agent_command_from_row)
            .map(|result| result.map(|command| command.view))
            .collect()
    }

    pub async fn claim_agent_command(
        &self,
        request: ClaimAgentCommand,
    ) -> Result<AgentCommandView, StoreError> {
        if !(1..=128).contains(&request.claimant.len())
            || !request.claimant.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(StoreError::AgentTransitionInvalid(
                "command claimant is invalid".to_owned(),
            ));
        }
        let request_digest = claim_request_digest(&request)?;
        let mut tx = self.pool().begin().await?;
        let aggregate =
            lock_agent_aggregate(&mut tx, &request.session_id, &request.task_id).await?;
        let row =
            select_agent_command_for_update(&mut tx, &request.task_id, &request.command_id).await?;
        let command = locked_agent_command_from_row(&row)?;
        if command.claim_idempotency_key.as_deref() == Some(request.idempotency_key.as_str()) {
            if command.claim_request_digest.as_deref() != Some(request_digest.as_str()) {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            return Ok(command.view);
        }
        if aggregate.task_lifecycle != TaskLifecycle::Running
            || aggregate.goal_phase != GoalPhase::Active
            || matches!(
                aggregate.operational_status,
                OperationalStatus::Paused
                    | OperationalStatus::Blocked
                    | OperationalStatus::Finished
            )
        {
            return Err(StoreError::RevisionConflict);
        }
        if command.view.state != AgentCommandState::Pending
            || command.view.revision != request.expected_command_revision
        {
            return Err(StoreError::RevisionConflict);
        }
        let row = sqlx::query(
            "UPDATE agent_commands SET state = 'claimed', claimant = $1, \
             claim_idempotency_key = $2, claim_request_digest = $3, \
             revision = revision + 1, updated_at = CURRENT_TIMESTAMP \
             WHERE id = $4 AND task_id = $5 AND state = 'pending' AND revision = $6 \
             RETURNING id, task_id, runtime_key, ordinal, schema_version, command_kind, \
                       payload, payload_digest, state, revision, claimant, \
                       claim_idempotency_key, claim_request_digest, \
                       completion_idempotency_key, completion_request_digest",
        )
        .bind(&request.claimant)
        .bind(request.idempotency_key.as_str())
        .bind(&request_digest)
        .bind(request.command_id.as_uuid())
        .bind(request.task_id.as_uuid())
        .bind(to_i64(
            request.expected_command_revision,
            "command revision is malformed",
        )?)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::RevisionConflict)?;
        let claimed = locked_agent_command_from_row(&row)?.view;
        let aggregate = locked_agent_aggregate_snapshot(&mut tx, &request.task_id).await?;
        append_agent_task_event(
            &mut tx,
            &request.task_id,
            AgentTaskEventKind::CommandClaimed,
            json!({
                "command_id": request.command_id,
                "command_key": claimed.key,
                "claimant": request.claimant,
                "command_revision": claimed.revision,
            }),
            aggregate.0,
            aggregate.1,
        )
        .await?;
        tx.commit().await?;
        Ok(claimed)
    }

    pub async fn load_agent_command_context(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
        command_id: AgentCommandId,
    ) -> Result<AgentCommandContext, StoreError> {
        let row = self.agent_row(&session_id, &task_id).await?;
        let command = sqlx::query(
            "SELECT id, task_id, runtime_key, ordinal, schema_version, command_kind, \
                    payload, payload_digest, state, revision, claimant, \
                    claim_idempotency_key, claim_request_digest, \
                    completion_idempotency_key, completion_request_digest \
             FROM agent_commands WHERE task_id = $1 AND id = $2",
        )
        .bind(task_id.as_uuid())
        .bind(command_id.as_uuid())
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::RevisionConflict)?;
        Ok(AgentCommandContext {
            execution: agent_execution_from_row(&row, self.agent_runtime_ceilings())?,
            definition: definition_from_agent_row(&row)?,
            command: locked_agent_command_from_row(&command)?.view,
        })
    }

    pub async fn replay_agent_command_completion(
        &self,
        principal: &PrincipalId,
        completion: &CompleteAgentCommand,
    ) -> Result<Option<AgentExecutionView>, StoreError> {
        authenticate_agent_owner(self, principal, &completion.session_id, &completion.task_id)
            .await?;
        let request_digest = completion_request_digest(completion)?;
        let row = sqlx::query(
            "SELECT completion_idempotency_key, completion_request_digest \
             FROM agent_commands WHERE task_id = $1 AND id = $2",
        )
        .bind(completion.task_id.as_uuid())
        .bind(completion.command_id.as_uuid())
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::RevisionConflict)?;
        if row
            .get::<Option<String>, _>("completion_idempotency_key")
            .as_deref()
            != Some(completion.idempotency_key.as_str())
        {
            return Ok(None);
        }
        if row
            .get::<Option<String>, _>("completion_request_digest")
            .as_deref()
            != Some(request_digest.as_str())
        {
            return Err(StoreError::IdempotencyConflict);
        }
        self.get_agent_execution(completion.session_id.clone(), completion.task_id.clone())
            .await
            .map(Some)
    }

    pub async fn commit_agent_command_transition(
        &self,
        principal: &PrincipalId,
        completion: &CompleteAgentCommand,
        transition: &RuntimeTransition,
        policy: Option<&crate::AgentPolicyDecision>,
        ceilings: &RuntimeCeilings,
    ) -> Result<AgentExecutionView, StoreError> {
        if ceilings != self.agent_runtime_ceilings() {
            return Err(StoreError::AgentTransitionInvalid(
                "runtime ceilings do not match deployment configuration".to_owned(),
            ));
        }
        let mut shape_execution = self
            .get_agent_execution(completion.session_id.clone(), completion.task_id.clone())
            .await?;
        let goal_contract = shape_execution.task.goal.clone();
        // Validate the proposal envelope without treating this unlocked read as
        // revision authority. The locked aggregate below resolves a concurrent
        // completion receipt before it performs the authoritative CAS checks.
        shape_execution.task_revision = transition.expected_task_revision;
        shape_execution.goal_revision = transition.expected_goal_revision;
        validate_transition(transition, &shape_execution, ceilings)
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        let event_value = serde_json::to_value(&completion.event).map_err(invalid_data)?;
        crate::canonical_json(&event_value)
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        let completion_digest = completion_request_digest(completion)?;
        let transition_digest = transition_request_digest(transition)?;
        let snapshot_bytes = transition
            .snapshot
            .canonical_bytes(ceilings)
            .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
        let snapshot_digest = transition
            .snapshot
            .digest(ceilings)
            .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
        let prepared_commands = prepare_commands(&transition.commands, ceilings)?;
        let prospective_evidence = prospective_harness_evidence(completion)?;

        let mut tx = self.pool().begin().await?;
        let aggregate =
            lock_agent_aggregate(&mut tx, &completion.session_id, &completion.task_id).await?;
        if aggregate.task_principal != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        let current_snapshot = lock_runtime_snapshot(
            &mut tx,
            &completion.task_id,
            aggregate.task_revision,
            ceilings,
        )
        .await?;
        let rows = lock_agent_commands(&mut tx, &completion.task_id).await?;
        let commands = rows
            .iter()
            .map(locked_agent_command_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let source = commands
            .iter()
            .find(|command| command.view.id == completion.command_id)
            .ok_or(StoreError::RevisionConflict)?;

        if source.completion_idempotency_key.as_deref() == Some(completion.idempotency_key.as_str())
        {
            if source.completion_request_digest.as_deref() != Some(completion_digest.as_str()) {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            return self
                .get_agent_execution(completion.session_id.clone(), completion.task_id.clone())
                .await;
        }
        validate_completion_locked(
            principal, completion, transition, policy, &aggregate, source,
        )?;
        let next_task_revision = aggregate
            .task_revision
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidData("user task revision is malformed".to_owned()))?;
        reconcile_transition_commands(
            &current_snapshot,
            &completion.event,
            transition,
            source,
            &commands,
            aggregate.task_revision,
            ceilings,
        )?;

        let authority = authority_command(transition);
        let mut evidence_ids = authority_evidence_ids(authority);
        if let Some(record) = prospective_evidence.as_ref() {
            evidence_ids.insert(record.id.clone());
        }
        let mut evidence =
            load_authorized_evidence(&mut tx, &completion.task_id, &evidence_ids).await?;
        if let Some(record) = prospective_evidence.as_ref() {
            if !matches!(source.view.payload, KernelCommandPayload::InvokeHarness(_)) {
                return Err(StoreError::GoalEvidenceUnauthorized);
            }
            if evidence.contains_key(&record.id) {
                return Err(StoreError::IdempotencyConflict);
            }
            evidence.insert(
                record.id.clone(),
                AuthorizedEvidence {
                    evidence_type: record.evidence_type.clone(),
                    source_authorized: true,
                    digest_current: true,
                },
            );
        }
        match authority {
            Some(KernelCommandPayload::CompleteGoal(command)) => {
                validate_complete_goal(&goal_contract, command, &evidence)?;
            }
            Some(KernelCommandPayload::BlockGoal(command)) => {
                validate_block_goal(&goal_contract, command, &evidence)?;
            }
            _ => {}
        }

        let next_goal_revision = if authority.is_some() {
            aggregate
                .goal_revision
                .checked_add(1)
                .ok_or_else(|| StoreError::InvalidData("goal revision is malformed".to_owned()))?
        } else {
            aggregate.goal_revision
        };
        let transition_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO agent_transitions \
             (id, task_id, idempotency_key, request_digest, source_event_id, \
              expected_task_revision, expected_goal_revision, committed_task_revision, \
              committed_goal_revision, snapshot_digest, command_digests) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(transition_id)
        .bind(completion.task_id.as_uuid())
        .bind(&transition.idempotency_key)
        .bind(&transition_digest)
        .bind(completion.event.event_id())
        .bind(to_i64(
            transition.expected_task_revision,
            "user task revision is malformed",
        )?)
        .bind(to_i64(
            transition.expected_goal_revision,
            "goal revision is malformed",
        )?)
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
        .bind(&snapshot_digest)
        .bind(
            serde_json::to_value(
                prepared_commands
                    .iter()
                    .map(|command| &command.command_digest)
                    .collect::<Vec<_>>(),
            )
            .map_err(invalid_data)?,
        )
        .execute(&mut *tx)
        .await?;
        if let (Some(payload), Some(policy)) = (authority, policy) {
            let action_kind = match payload {
                KernelCommandPayload::CompleteGoal(_) => "complete_goal",
                KernelCommandPayload::BlockGoal(_) => "block_goal",
                _ => unreachable!("authority_command returns only goal commands"),
            };
            sqlx::query(
                "INSERT INTO agent_policy_decisions \
                 (id, task_id, transition_id, action_kind, allowed, policy_revision, reason_code) \
                 VALUES ($1, $2, $3, $4, TRUE, $5, $6)",
            )
            .bind(uuid::Uuid::now_v7())
            .bind(completion.task_id.as_uuid())
            .bind(transition_id)
            .bind(action_kind)
            .bind(to_i64(
                policy.policy_revision.get(),
                "policy revision is malformed",
            )?)
            .bind(&policy.reason_code)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE agent_commands SET state = 'completed', completion_idempotency_key = $1, \
             completion_request_digest = $2, result = $3, revision = revision + 1, \
             updated_at = CURRENT_TIMESTAMP WHERE task_id = $4 AND id = $5 \
             AND state = 'claimed' AND revision = $6",
        )
        .bind(completion.idempotency_key.as_str())
        .bind(&completion_digest)
        .bind(&event_value)
        .bind(completion.task_id.as_uuid())
        .bind(completion.command_id.as_uuid())
        .bind(to_i64(
            completion.expected_command_revision,
            "command revision is malformed",
        )?)
        .execute(&mut *tx)
        .await?;
        if let Some(record) = prospective_evidence.as_ref() {
            let EvidenceSourceIdentity::BuiltinHarness { command_id } = &record.source;
            let inserted = sqlx::query(
                "INSERT INTO goal_evidence \
                 (id, task_id, evidence_type, source_kind, source_identity, content_digest) \
                 VALUES ($1, $2, $3, 'builtin_harness', $4, $5) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(record.id.as_uuid())
            .bind(record.task_id.as_uuid())
            .bind(&record.evidence_type)
            .bind(command_id.as_uuid())
            .bind(&record.digest)
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() != 1 {
                return Err(StoreError::IdempotencyConflict);
            }
        }
        sqlx::query(
            "UPDATE agent_runtime_snapshots SET task_revision = $1, schema_version = $2, \
             runtime_id = $3, disposition = $4, payload = $5, ceiling_bytes = $6, \
             canonical_bytes = $7, snapshot_digest = $8, updated_at = CURRENT_TIMESTAMP \
             WHERE task_id = $9",
        )
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(i32::from(transition.snapshot.schema_version))
        .bind(&transition.snapshot.runtime_id)
        .bind(runtime_disposition_name(transition.snapshot.disposition))
        .bind(&transition.snapshot.payload)
        .bind(
            i32::try_from(transition.snapshot.ceiling_bytes).map_err(|_| {
                StoreError::RuntimeStateInvalid("snapshot ceiling is malformed".to_owned())
            })?,
        )
        .bind(i32::try_from(snapshot_bytes.len()).map_err(|_| {
            StoreError::RuntimeStateInvalid("snapshot byte count is malformed".to_owned())
        })?)
        .bind(&snapshot_digest)
        .bind(completion.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;

        for (ordinal, prepared) in prepared_commands.iter().enumerate() {
            let completed = matches!(
                prepared.command.payload,
                KernelCommandPayload::CompleteGoal(_)
                    | KernelCommandPayload::BlockGoal(_)
                    | KernelCommandPayload::FailTask(_)
            );
            sqlx::query(
                "INSERT INTO agent_commands \
                 (id, task_id, transition_id, ordinal, schema_version, runtime_key, \
                  command_kind, payload, payload_digest, state, revision, result) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            )
            .bind(uuid::Uuid::now_v7())
            .bind(completion.task_id.as_uuid())
            .bind(transition_id)
            .bind(i32::try_from(ordinal).map_err(|_| {
                StoreError::AgentTransitionInvalid("command ordinal is invalid".to_owned())
            })?)
            .bind(i32::from(prepared.command.schema_version))
            .bind(prepared.command.key.as_str())
            .bind(command_kind(&prepared.command.payload))
            .bind(&prepared.payload)
            .bind(&prepared.payload_digest)
            .bind(if completed { "completed" } else { "pending" })
            .bind(if completed { 1_i64 } else { 0_i64 })
            .bind(completed.then_some(&prepared.payload))
            .execute(&mut *tx)
            .await?;
        }

        match authority {
            Some(KernelCommandPayload::CompleteGoal(command)) => {
                let mut assertions = command.assertions.iter().collect::<Vec<_>>();
                assertions.sort_by(|left, right| left.criterion_id.cmp(&right.criterion_id));
                for assertion in assertions {
                    sqlx::query(
                        "INSERT INTO goal_criterion_assertions \
                         (task_id, criterion_id, evidence_id, goal_revision) \
                         VALUES ($1, $2, $3, $4)",
                    )
                    .bind(completion.task_id.as_uuid())
                    .bind(&assertion.criterion_id)
                    .bind(assertion.evidence_id.as_uuid())
                    .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
                    .execute(&mut *tx)
                    .await?;
                }
                sqlx::query(
                    "UPDATE goal_states SET phase = 'complete', revision = $1, \
                     current_blocker_id = NULL, updated_at = CURRENT_TIMESTAMP \
                     WHERE task_id = $2",
                )
                .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
                .bind(completion.task_id.as_uuid())
                .execute(&mut *tx)
                .await?;
            }
            Some(KernelCommandPayload::BlockGoal(command)) => {
                let blocker_id = uuid::Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO goal_blockers \
                     (id, task_id, blocked_when_index, reason_code, explanation, evidence_ids, \
                      unlock_conditions, goal_revision) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(blocker_id)
                .bind(completion.task_id.as_uuid())
                .bind(i32::from(command.blocked_when_index))
                .bind(&command.reason_code)
                .bind(&command.explanation)
                .bind(serde_json::to_value(&command.evidence_ids).map_err(invalid_data)?)
                .bind(serde_json::to_value(&command.unlock_conditions).map_err(invalid_data)?)
                .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE goal_states SET phase = 'blocked', revision = $1, \
                     current_blocker_id = $2, updated_at = CURRENT_TIMESTAMP WHERE task_id = $3",
                )
                .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
                .bind(blocker_id)
                .bind(completion.task_id.as_uuid())
                .execute(&mut *tx)
                .await?;
            }
            _ => {}
        }

        let (task_state, status, failure) = transition_task_state(transition, ceilings)?;
        if task_state != "running" {
            sqlx::query(
                "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1, \
                 updated_at = CURRENT_TIMESTAMP WHERE task_id = $1 AND id <> $2 \
                 AND state IN ('pending', 'claimed')",
            )
            .bind(completion.task_id.as_uuid())
            .bind(completion.command_id.as_uuid())
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE user_tasks SET state = $1, operational_status = $2, revision = $3, \
             failure = $4, finished_at = CASE WHEN $1 = 'running' THEN NULL \
                 ELSE CURRENT_TIMESTAMP END, updated_at = CURRENT_TIMESTAMP WHERE id = $5",
        )
        .bind(task_state)
        .bind(operational_status_name(status))
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(failure)
        .bind(completion.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        let event_kind = match authority {
            Some(KernelCommandPayload::CompleteGoal(_)) => AgentTaskEventKind::GoalCompleted,
            Some(KernelCommandPayload::BlockGoal(_)) => AgentTaskEventKind::GoalBlocked,
            _ => AgentTaskEventKind::RuntimeTransitionCommitted,
        };
        let mut event_metadata = json!({
            "transition_id": transition_id,
            "source_event_id": completion.event.event_id(),
            "source_command_id": completion.command_id,
            "snapshot_digest": snapshot_digest,
            "command_count": transition.commands.len(),
        });
        if let Some(policy) = policy {
            let metadata = event_metadata
                .as_object_mut()
                .expect("agent event metadata is an object");
            metadata.insert(
                "policy_revision".to_owned(),
                json!(policy.policy_revision.get()),
            );
            metadata.insert("reason_code".to_owned(), json!(policy.reason_code));
        }
        append_agent_task_event(
            &mut tx,
            &completion.task_id,
            event_kind,
            event_metadata,
            next_task_revision,
            next_goal_revision,
        )
        .await?;
        tx.commit().await?;
        self.get_agent_execution(completion.session_id.clone(), completion.task_id.clone())
            .await
    }

    pub async fn pause_user_task(
        &self,
        principal: &PrincipalId,
        request: PauseUserTask,
    ) -> Result<AgentExecutionView, StoreError> {
        let request_digest = pause_request_digest(&request)?;
        let mut tx = self.pool().begin().await?;
        let aggregate =
            lock_agent_aggregate(&mut tx, &request.session_id, &request.task_id).await?;
        if aggregate.task_principal != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if let Some(replay) = load_action_replay(
            &mut tx,
            &request.task_id,
            "pause",
            request.idempotency_key.as_str(),
        )
        .await?
        {
            if replay.request_digest != request_digest {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            return self
                .get_agent_execution(request.session_id, request.task_id)
                .await;
        }
        validate_principal_action_revisions(
            &aggregate,
            &request.expected_task_revision,
            &request.expected_goal_revision,
        )?;
        if aggregate.goal_phase == GoalPhase::Complete {
            return Err(StoreError::GoalAlreadyTerminal);
        }
        if aggregate.task_lifecycle != TaskLifecycle::Running {
            return Err(StoreError::AgentTaskAlreadyTerminal);
        }
        if aggregate.goal_phase != GoalPhase::Active
            || !goal_edge_allowed(aggregate.goal_phase, GoalPhase::Paused)
        {
            return Err(StoreError::AgentTransitionInvalid(
                "pause requires an active goal".to_owned(),
            ));
        }
        if !bounded_action_text(&request.reason) {
            return Err(StoreError::AgentTransitionInvalid(
                "pause reason is invalid".to_owned(),
            ));
        }
        lock_optional_runtime_snapshot(
            &mut tx,
            &request.task_id,
            aggregate.task_revision,
            self.agent_runtime_ceilings(),
        )
        .await?
        .ok_or_else(snapshot_integrity_error)?;
        let next_task_revision = next_revision(aggregate.task_revision, "user task revision")?;
        let next_goal_revision = next_revision(aggregate.goal_revision, "goal revision")?;
        advance_runtime_snapshot_revision(&mut tx, &request.task_id, next_task_revision).await?;
        sqlx::query(
            "UPDATE goal_states SET phase = 'paused', revision = $1, \
             updated_at = CURRENT_TIMESTAMP WHERE task_id = $2",
        )
        .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE user_tasks SET operational_status = 'paused', revision = $1, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $2",
        )
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        insert_agent_action(
            &mut tx,
            &request.task_id,
            "pause",
            request.idempotency_key.as_str(),
            &request_digest,
            next_task_revision,
            next_goal_revision,
        )
        .await?;
        append_agent_task_event(
            &mut tx,
            &request.task_id,
            AgentTaskEventKind::TaskPaused,
            json!({"reason": request.reason}),
            next_task_revision,
            next_goal_revision,
        )
        .await?;
        tx.commit().await?;
        self.get_agent_execution(request.session_id, request.task_id)
            .await
    }

    pub async fn resume_user_task(
        &self,
        principal: &PrincipalId,
        request: ResumeUserTask,
        blocked_transition: Option<&RuntimeTransition>,
        ceilings: &RuntimeCeilings,
    ) -> Result<AgentExecutionView, StoreError> {
        if ceilings != self.agent_runtime_ceilings() {
            return Err(StoreError::AgentTransitionInvalid(
                "runtime ceilings do not match deployment configuration".to_owned(),
            ));
        }
        let request_digest = resume_request_digest(&request)?;
        let prepared = blocked_transition
            .map(|transition| prepare_commands(&transition.commands, ceilings))
            .transpose()?;
        let transition_digest = blocked_transition
            .map(transition_request_digest)
            .transpose()?;
        let snapshot_material = blocked_transition
            .map(|transition| {
                Ok::<_, StoreError>((
                    transition
                        .snapshot
                        .canonical_bytes(ceilings)
                        .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?,
                    transition
                        .snapshot
                        .digest(ceilings)
                        .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?,
                ))
            })
            .transpose()?;

        let mut tx = self.pool().begin().await?;
        let aggregate =
            lock_agent_aggregate(&mut tx, &request.session_id, &request.task_id).await?;
        if aggregate.task_principal != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if let Some(replay) = load_action_replay(
            &mut tx,
            &request.task_id,
            "resume",
            request.idempotency_key.as_str(),
        )
        .await?
        {
            if replay.request_digest != request_digest {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            return self
                .get_agent_execution(request.session_id, request.task_id)
                .await;
        }
        validate_principal_action_revisions(
            &aggregate,
            &request.expected_task_revision,
            &request.expected_goal_revision,
        )?;
        if aggregate.goal_phase == GoalPhase::Complete {
            return Err(StoreError::GoalAlreadyTerminal);
        }
        if aggregate.task_lifecycle != TaskLifecycle::Running {
            return Err(StoreError::AgentTaskAlreadyTerminal);
        }
        let current_snapshot = lock_optional_runtime_snapshot(
            &mut tx,
            &request.task_id,
            aggregate.task_revision,
            ceilings,
        )
        .await?
        .ok_or_else(snapshot_integrity_error)?;
        let rows = lock_agent_commands(&mut tx, &request.task_id).await?;
        let commands = rows
            .iter()
            .map(locked_agent_command_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_task_revision = next_revision(aggregate.task_revision, "user task revision")?;
        let next_goal_revision = next_revision(aggregate.goal_revision, "goal revision")?;

        let status = match aggregate.goal_phase {
            GoalPhase::Paused => {
                if blocked_transition.is_some()
                    || !goal_edge_allowed(GoalPhase::Paused, GoalPhase::Active)
                {
                    return Err(StoreError::AgentTransitionInvalid(
                        "paused resume cannot replace the runtime snapshot".to_owned(),
                    ));
                }
                advance_runtime_snapshot_revision(&mut tx, &request.task_id, next_task_revision)
                    .await?;
                outstanding_operational_status(&commands)?
            }
            GoalPhase::Blocked => {
                let transition = blocked_transition.ok_or_else(|| {
                    StoreError::AgentTransitionInvalid(
                        "blocked resume requires a runtime transition".to_owned(),
                    )
                })?;
                let prepared = prepared.as_ref().expect("blocked transition was prepared");
                let (snapshot_bytes, snapshot_digest) = snapshot_material
                    .as_ref()
                    .expect("blocked transition snapshot was prepared");
                validate_blocked_resume_transition(
                    &request,
                    &aggregate,
                    &current_snapshot,
                    &commands,
                    transition,
                    ceilings,
                )?;
                let transition_id = uuid::Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO agent_transitions \
                     (id, task_id, idempotency_key, request_digest, source_event_id, \
                      expected_task_revision, expected_goal_revision, committed_task_revision, \
                      committed_goal_revision, snapshot_digest, command_digests) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                )
                .bind(transition_id)
                .bind(request.task_id.as_uuid())
                .bind(&transition.idempotency_key)
                .bind(
                    transition_digest
                        .as_ref()
                        .expect("transition digest exists"),
                )
                .bind(principal_resumed_event_id(
                    &request.task_id,
                    &request.idempotency_key,
                ))
                .bind(to_i64(
                    aggregate.task_revision,
                    "user task revision is malformed",
                )?)
                .bind(to_i64(
                    aggregate.goal_revision,
                    "goal revision is malformed",
                )?)
                .bind(to_i64(
                    next_task_revision,
                    "user task revision is malformed",
                )?)
                .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
                .bind(snapshot_digest)
                .bind(
                    serde_json::to_value(
                        prepared
                            .iter()
                            .map(|command| &command.command_digest)
                            .collect::<Vec<_>>(),
                    )
                    .map_err(invalid_data)?,
                )
                .execute(&mut *tx)
                .await?;
                replace_runtime_snapshot(
                    &mut tx,
                    &request.task_id,
                    next_task_revision,
                    &transition.snapshot,
                    snapshot_bytes.len(),
                    snapshot_digest,
                )
                .await?;
                insert_prepared_commands(&mut tx, &request.task_id, transition_id, prepared)
                    .await?;
                transition_task_state(transition, ceilings)?.1
            }
            GoalPhase::Active => {
                return Err(StoreError::AgentTransitionInvalid(
                    "resume requires a paused or blocked goal".to_owned(),
                ));
            }
            GoalPhase::Complete => unreachable!("complete was rejected above"),
        };

        sqlx::query(
            "UPDATE goal_states SET phase = 'active', revision = $1, \
             current_blocker_id = NULL, updated_at = CURRENT_TIMESTAMP WHERE task_id = $2",
        )
        .bind(to_i64(next_goal_revision, "goal revision is malformed")?)
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE user_tasks SET operational_status = $1, revision = $2, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $3",
        )
        .bind(operational_status_name(status))
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        insert_agent_action(
            &mut tx,
            &request.task_id,
            "resume",
            request.idempotency_key.as_str(),
            &request_digest,
            next_task_revision,
            next_goal_revision,
        )
        .await?;
        append_agent_task_event(
            &mut tx,
            &request.task_id,
            AgentTaskEventKind::TaskResumed,
            json!({"from": goal_phase_name(aggregate.goal_phase)}),
            next_task_revision,
            next_goal_revision,
        )
        .await?;
        tx.commit().await?;
        self.get_agent_execution(request.session_id, request.task_id)
            .await
    }

    pub async fn cancel_user_task(
        &self,
        principal: &PrincipalId,
        request: CancelUserTask,
    ) -> Result<AgentExecutionView, StoreError> {
        let request_digest = cancel_request_digest(&request)?;
        let mut tx = self.pool().begin().await?;
        let aggregate =
            lock_agent_aggregate(&mut tx, &request.session_id, &request.task_id).await?;
        if aggregate.task_principal != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if let Some(replay) = load_action_replay(
            &mut tx,
            &request.task_id,
            "cancel",
            request.idempotency_key.as_str(),
        )
        .await?
        {
            if replay.request_digest != request_digest {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            return self
                .get_agent_execution(request.session_id, request.task_id)
                .await;
        }
        validate_principal_action_revisions(
            &aggregate,
            &request.expected_task_revision,
            &request.expected_goal_revision,
        )?;
        if aggregate.goal_phase == GoalPhase::Complete {
            return Err(StoreError::GoalAlreadyTerminal);
        }
        if !matches!(
            aggregate.task_lifecycle,
            TaskLifecycle::Accepted | TaskLifecycle::Running
        ) {
            return Err(StoreError::AgentTaskAlreadyTerminal);
        }
        if request
            .reason
            .as_deref()
            .is_some_and(|reason| !bounded_action_text(reason))
        {
            return Err(StoreError::AgentTransitionInvalid(
                "cancellation reason is invalid".to_owned(),
            ));
        }
        let snapshot = lock_optional_runtime_snapshot(
            &mut tx,
            &request.task_id,
            aggregate.task_revision,
            self.agent_runtime_ceilings(),
        )
        .await?;
        let _commands = lock_agent_commands(&mut tx, &request.task_id).await?;
        let next_task_revision = next_revision(aggregate.task_revision, "user task revision")?;
        sqlx::query(
            "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1, \
             updated_at = CURRENT_TIMESTAMP WHERE task_id = $1 \
             AND state IN ('pending', 'claimed')",
        )
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        if snapshot.is_some() {
            advance_runtime_snapshot_revision(&mut tx, &request.task_id, next_task_revision)
                .await?;
        }
        sqlx::query(
            "UPDATE user_tasks SET state = 'cancelled', operational_status = 'finished', \
             revision = $1, cancellation_reason = $2, finished_at = CURRENT_TIMESTAMP, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $3",
        )
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(&request.reason)
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        insert_agent_action(
            &mut tx,
            &request.task_id,
            "cancel",
            request.idempotency_key.as_str(),
            &request_digest,
            next_task_revision,
            aggregate.goal_revision,
        )
        .await?;
        append_agent_task_event(
            &mut tx,
            &request.task_id,
            AgentTaskEventKind::TaskCancelled,
            json!({"reason": request.reason}),
            next_task_revision,
            aggregate.goal_revision,
        )
        .await?;
        tx.commit().await?;
        self.get_agent_execution(request.session_id, request.task_id)
            .await
    }

    pub async fn fail_unavailable_agent_command(
        &self,
        principal: &PrincipalId,
        request: &FailAgentCommand,
    ) -> Result<AgentExecutionView, StoreError> {
        if request.code != "runtime_command_unavailable" {
            return Err(StoreError::AgentTransitionInvalid(
                "unavailable command failure code is invalid".to_owned(),
            ));
        }
        let request_digest = unavailable_request_digest(request)?;
        let failure = json!({"code": "runtime_command_unavailable"});
        let mut tx = self.pool().begin().await?;
        let aggregate =
            lock_agent_aggregate(&mut tx, &request.session_id, &request.task_id).await?;
        if aggregate.task_principal != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        let current_snapshot = lock_runtime_snapshot(
            &mut tx,
            &request.task_id,
            aggregate.task_revision,
            self.agent_runtime_ceilings(),
        )
        .await?;
        let rows = lock_agent_commands(&mut tx, &request.task_id).await?;
        let commands = rows
            .iter()
            .map(locked_agent_command_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let source = commands
            .iter()
            .find(|command| command.view.id == request.command_id)
            .ok_or(StoreError::RevisionConflict)?;

        if source.completion_idempotency_key.as_deref() == Some(request.idempotency_key.as_str()) {
            if source.completion_request_digest.as_deref() != Some(request_digest.as_str()) {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            return self
                .get_agent_execution(request.session_id.clone(), request.task_id.clone())
                .await;
        }
        if aggregate.task_lifecycle != TaskLifecycle::Running
            || aggregate.operational_status == OperationalStatus::Finished
        {
            return Err(StoreError::AgentTaskAlreadyTerminal);
        }
        if aggregate.goal_phase != GoalPhase::Active {
            return Err(StoreError::RevisionConflict);
        }
        if source.view.state != AgentCommandState::Claimed
            || source.view.revision != request.expected_command_revision
        {
            return Err(StoreError::RevisionConflict);
        }
        let state = KiteAgentRuntime::new(*self.agent_runtime_ceilings())
            .decode_snapshot_for_task_revision(&current_snapshot, aggregate.task_revision)
            .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
        let waits = runtime_waits(&state)?;
        let source_kind = external_command_kind(&source.view.payload).ok_or_else(|| {
            StoreError::AgentTransitionInvalid("source command is not external work".to_owned())
        })?;
        if waits.get(source.view.key.as_str()) != Some(&source_kind) {
            return Err(StoreError::AgentTransitionInvalid(
                "source command does not match the current snapshot wait".to_owned(),
            ));
        }

        let next_task_revision = aggregate
            .task_revision
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidData("user task revision is malformed".to_owned()))?;
        let updated = sqlx::query(
            "UPDATE agent_commands SET state = 'failed', completion_idempotency_key = $1, \
             completion_request_digest = $2, failure = $3, revision = revision + 1, \
             updated_at = CURRENT_TIMESTAMP WHERE task_id = $4 AND id = $5 \
             AND state = 'claimed' AND revision = $6",
        )
        .bind(request.idempotency_key.as_str())
        .bind(&request_digest)
        .bind(&failure)
        .bind(request.task_id.as_uuid())
        .bind(request.command_id.as_uuid())
        .bind(to_i64(
            request.expected_command_revision,
            "command revision is malformed",
        )?)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(StoreError::RevisionConflict);
        }
        sqlx::query(
            "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1, \
             updated_at = CURRENT_TIMESTAMP WHERE task_id = $1 AND id <> $2 \
             AND state IN ('pending', 'claimed')",
        )
        .bind(request.task_id.as_uuid())
        .bind(request.command_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_runtime_snapshots SET task_revision = $1, \
             updated_at = CURRENT_TIMESTAMP WHERE task_id = $2",
        )
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE user_tasks SET state = 'failed', operational_status = 'finished', \
             revision = $1, failure = $2, finished_at = CURRENT_TIMESTAMP, \
             updated_at = CURRENT_TIMESTAMP WHERE id = $3",
        )
        .bind(to_i64(
            next_task_revision,
            "user task revision is malformed",
        )?)
        .bind(&failure)
        .bind(request.task_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        append_agent_task_event(
            &mut tx,
            &request.task_id,
            AgentTaskEventKind::RuntimeCommandUnavailable,
            json!({
                "command_id": request.command_id,
                "command_key": source.view.key,
                "code": "runtime_command_unavailable",
            }),
            next_task_revision,
            aggregate.goal_revision,
        )
        .await?;
        tx.commit().await?;
        self.get_agent_execution(request.session_id.clone(), request.task_id.clone())
            .await
    }

    pub async fn recover_agent_commands(
        &self,
        limit: i64,
    ) -> Result<AgentRecoveryReport, StoreError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let tasks = sqlx::query(
            "SELECT DISTINCT t.id, t.revision \
             FROM user_tasks t JOIN agent_commands c ON c.task_id = t.id \
             WHERE t.state = 'running' AND c.state IN ('pending', 'claimed') \
             ORDER BY t.id LIMIT $1",
        )
        .bind(limit.clamp(1, 256))
        .fetch_all(&mut *tx)
        .await?;
        let mut report = AgentRecoveryReport {
            pending: 0,
            claimed: 0,
        };
        for task in tasks {
            let task_id = UserTaskId::new(task.get("id"));
            let task_revision =
                nonnegative(task.get("revision"), "user task revision is malformed")?;
            let snapshot = read_runtime_snapshot(
                &mut tx,
                &task_id,
                task_revision,
                self.agent_runtime_ceilings(),
            )
            .await?;
            let state = KiteAgentRuntime::new(*self.agent_runtime_ceilings())
                .decode_snapshot_for_task_revision(&snapshot, task_revision)
                .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
            let waits = runtime_waits(&state)?;
            let rows = sqlx::query(
                "SELECT id, task_id, runtime_key, ordinal, schema_version, command_kind, payload, \
                        payload_digest, state, revision, claimant, claim_idempotency_key, \
                        claim_request_digest, completion_idempotency_key, \
                        completion_request_digest \
                 FROM agent_commands WHERE task_id = $1 \
                 ORDER BY id",
            )
            .bind(task_id.as_uuid())
            .fetch_all(&mut *tx)
            .await?;
            let commands = rows
                .iter()
                .map(locked_agent_command_from_row)
                .collect::<Result<Vec<_>, _>>()?;
            let mut command_waits = BTreeMap::new();
            for command in commands {
                if command.view.task_id != task_id {
                    return Err(StoreError::RuntimeStateInvalid(
                        "recovered command belongs to another task".to_owned(),
                    ));
                }
                match command.view.state {
                    AgentCommandState::Pending => {
                        record_recovered_wait(&waits, &mut command_waits, &command)?;
                        report.pending = report.pending.checked_add(1).ok_or_else(|| {
                            StoreError::InvalidData(
                                "recovery command count is malformed".to_owned(),
                            )
                        })?;
                    }
                    AgentCommandState::Claimed => {
                        record_recovered_wait(&waits, &mut command_waits, &command)?;
                        report.claimed = report.claimed.checked_add(1).ok_or_else(|| {
                            StoreError::InvalidData(
                                "recovery command count is malformed".to_owned(),
                            )
                        })?;
                    }
                    AgentCommandState::Completed
                    | AgentCommandState::Failed
                    | AgentCommandState::Cancelled => {}
                }
            }
            if waits != command_waits {
                return Err(StoreError::RuntimeStateInvalid(
                    "recovered snapshot wait has no outstanding command".to_owned(),
                ));
            }
        }
        tx.commit().await?;
        Ok(report)
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

struct PreparedCommand<'a> {
    command: &'a KernelCommand,
    payload: Value,
    payload_digest: String,
    command_digest: String,
}

async fn authenticate_agent_owner(
    store: &PostgresStore,
    principal: &PrincipalId,
    session_id: &SessionId,
    task_id: &UserTaskId,
) -> Result<(), StoreError> {
    let row = sqlx::query(
        "SELECT s.principal AS session_principal, s.state AS session_state, \
                t.principal AS task_principal \
         FROM sessions s JOIN user_tasks t ON t.session_id = s.id \
         WHERE s.id = $1 AND t.id = $2",
    )
    .bind(session_id.as_uuid())
    .bind(task_id.as_uuid())
    .fetch_optional(store.pool())
    .await?
    .ok_or(StoreError::UserTaskNotFound)?;
    if row.get::<String, _>("session_principal") != principal.as_str()
        || row.get::<String, _>("task_principal") != principal.as_str()
    {
        return Err(StoreError::PrincipalMismatch);
    }
    if row.get::<String, _>("session_state") != "active" {
        return Err(StoreError::SessionClosed);
    }
    Ok(())
}

async fn select_agent_command_for_update(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    command_id: &AgentCommandId,
) -> Result<PgRow, StoreError> {
    sqlx::query(
        "SELECT id, task_id, runtime_key, ordinal, schema_version, command_kind, payload, \
                payload_digest, state, revision, claimant, claim_idempotency_key, \
                claim_request_digest, completion_idempotency_key, completion_request_digest \
         FROM agent_commands WHERE task_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(task_id.as_uuid())
    .bind(command_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::RevisionConflict)
}

async fn lock_agent_commands(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
) -> Result<Vec<PgRow>, StoreError> {
    Ok(sqlx::query(
        "SELECT id, task_id, runtime_key, ordinal, schema_version, command_kind, payload, \
                payload_digest, state, revision, claimant, claim_idempotency_key, \
                claim_request_digest, completion_idempotency_key, completion_request_digest \
         FROM agent_commands WHERE task_id = $1 ORDER BY id FOR UPDATE",
    )
    .bind(task_id.as_uuid())
    .fetch_all(&mut **tx)
    .await?)
}

async fn locked_agent_aggregate_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
) -> Result<(u64, u64), StoreError> {
    let row = sqlx::query(
        "SELECT t.revision AS task_revision, g.revision AS goal_revision \
         FROM user_tasks t JOIN goal_states g ON g.task_id = t.id WHERE t.id = $1",
    )
    .bind(task_id.as_uuid())
    .fetch_one(&mut **tx)
    .await?;
    Ok((
        nonnegative(row.get("task_revision"), "user task revision is malformed")?,
        nonnegative(row.get("goal_revision"), "goal revision is malformed")?,
    ))
}

async fn lock_runtime_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    task_revision: u64,
    ceilings: &RuntimeCeilings,
) -> Result<RuntimeStateSnapshot, StoreError> {
    let row = sqlx::query(
        "SELECT task_revision, schema_version, runtime_id, disposition, payload, \
                ceiling_bytes, canonical_bytes, snapshot_digest \
         FROM agent_runtime_snapshots WHERE task_id = $1 FOR UPDATE",
    )
    .bind(task_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(snapshot_integrity_error)?;
    runtime_snapshot_from_locked_row(&row, task_revision, ceilings)
}

async fn read_runtime_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    task_revision: u64,
    ceilings: &RuntimeCeilings,
) -> Result<RuntimeStateSnapshot, StoreError> {
    let row = sqlx::query(
        "SELECT task_revision, schema_version, runtime_id, disposition, payload, \
                ceiling_bytes, canonical_bytes, snapshot_digest \
         FROM agent_runtime_snapshots WHERE task_id = $1",
    )
    .bind(task_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(snapshot_integrity_error)?;
    runtime_snapshot_from_locked_row(&row, task_revision, ceilings)
}

fn runtime_snapshot_from_locked_row(
    row: &PgRow,
    task_revision: u64,
    ceilings: &RuntimeCeilings,
) -> Result<RuntimeStateSnapshot, StoreError> {
    if nonnegative_runtime(row.get("task_revision"))? != task_revision {
        return Err(snapshot_integrity_error());
    }
    let snapshot = RuntimeStateSnapshot {
        schema_version: u16::try_from(row.get::<i32, _>("schema_version"))
            .map_err(|_| snapshot_integrity_error())?,
        runtime_id: row.get("runtime_id"),
        disposition: runtime_disposition(&row.get::<String, _>("disposition"))?,
        payload: row.get("payload"),
        ceiling_bytes: u32::try_from(row.get::<i32, _>("ceiling_bytes"))
            .map_err(|_| snapshot_integrity_error())?,
        canonical_byte_count: u32::try_from(row.get::<i32, _>("canonical_bytes"))
            .map_err(|_| snapshot_integrity_error())?,
        snapshot_digest: row.get("snapshot_digest"),
    };
    snapshot
        .canonical_bytes(ceilings)
        .map_err(|_| snapshot_integrity_error())?;
    Ok(snapshot)
}

fn locked_agent_command_from_row(row: &PgRow) -> Result<LockedAgentCommand, StoreError> {
    let payload: Value = row.get("payload");
    let payload_digest: String = row.get("payload_digest");
    if canonical_sha256(&payload).map_err(|_| command_integrity_error())? != payload_digest {
        return Err(command_integrity_error());
    }
    let kind: String = row.get("command_kind");
    let payload = parse_command_payload(&kind, payload)?;
    let schema_version = u16::try_from(row.get::<i32, _>("schema_version"))
        .map_err(|_| command_integrity_error())?;
    if schema_version != 1 {
        return Err(command_integrity_error());
    }
    let key = RuntimeCommandKey::new(row.get::<String, _>("runtime_key"))
        .map_err(|_| command_integrity_error())?;
    Ok(LockedAgentCommand {
        view: AgentCommandView {
            id: AgentCommandId::new(row.get("id")),
            task_id: UserTaskId::new(row.get("task_id")),
            key,
            ordinal: u16::try_from(row.get::<i32, _>("ordinal"))
                .map_err(|_| command_integrity_error())?,
            schema_version,
            payload,
            payload_digest,
            state: agent_command_state(&row.get::<String, _>("state"))?,
            revision: nonnegative(row.get("revision"), "command revision is malformed")?,
            claimant: row.get("claimant"),
        },
        claim_idempotency_key: row.get("claim_idempotency_key"),
        claim_request_digest: row.get("claim_request_digest"),
        completion_idempotency_key: row.get("completion_idempotency_key"),
        completion_request_digest: row.get("completion_request_digest"),
    })
}

fn parse_command_payload(kind: &str, value: Value) -> Result<KernelCommandPayload, StoreError> {
    let parsed = match kind {
        "request_context" => {
            serde_json::from_value(value).map(KernelCommandPayload::RequestContext)
        }
        "wait_for_event" => serde_json::from_value(value).map(KernelCommandPayload::WaitForEvent),
        "invoke_harness" => serde_json::from_value(value).map(KernelCommandPayload::InvokeHarness),
        "request_approval" => {
            serde_json::from_value(value).map(KernelCommandPayload::RequestApproval)
        }
        "complete_goal" => serde_json::from_value(value).map(KernelCommandPayload::CompleteGoal),
        "block_goal" => serde_json::from_value(value).map(KernelCommandPayload::BlockGoal),
        "fail_task" => serde_json::from_value(value).map(KernelCommandPayload::FailTask),
        _ => return Err(command_integrity_error()),
    };
    parsed.map_err(|_| command_integrity_error())
}

fn agent_command_state(value: &str) -> Result<AgentCommandState, StoreError> {
    match value {
        "pending" => Ok(AgentCommandState::Pending),
        "claimed" => Ok(AgentCommandState::Claimed),
        "completed" => Ok(AgentCommandState::Completed),
        "failed" => Ok(AgentCommandState::Failed),
        "cancelled" => Ok(AgentCommandState::Cancelled),
        _ => Err(command_integrity_error()),
    }
}

fn claim_request_digest(request: &ClaimAgentCommand) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "sessionId": &request.session_id,
        "taskId": &request.task_id,
        "commandId": &request.command_id,
        "expectedCommandRevision": request.expected_command_revision,
        "claimant": &request.claimant,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn completion_request_digest(request: &CompleteAgentCommand) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "sessionId": &request.session_id,
        "taskId": &request.task_id,
        "commandId": &request.command_id,
        "expectedCommandRevision": request.expected_command_revision,
        "event": &request.event,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn unavailable_request_digest(request: &FailAgentCommand) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "sessionId": &request.session_id,
        "taskId": &request.task_id,
        "commandId": &request.command_id,
        "expectedCommandRevision": request.expected_command_revision,
        "code": &request.code,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn transition_request_digest(transition: &RuntimeTransition) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "expectedTaskRevision": transition.expected_task_revision,
        "expectedGoalRevision": transition.expected_goal_revision,
        "snapshot": &transition.snapshot,
        "commands": &transition.commands,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn prepare_commands<'a>(
    commands: &'a [KernelCommand],
    ceilings: &RuntimeCeilings,
) -> Result<Vec<PreparedCommand<'a>>, StoreError> {
    commands
        .iter()
        .map(|command| {
            let bytes = crate::canonical_json(command)
                .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
            if bytes.len() > ceilings.max_command_bytes {
                return Err(StoreError::AgentTransitionInvalid(
                    "runtime command ceiling exceeded".to_owned(),
                ));
            }
            let payload = command_payload(&command.payload)?;
            Ok(PreparedCommand {
                command,
                payload_digest: canonical_sha256(&payload)
                    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?,
                command_digest: canonical_sha256(command)
                    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?,
                payload,
            })
        })
        .collect()
}

fn validate_completion_locked(
    principal: &PrincipalId,
    completion: &CompleteAgentCommand,
    transition: &RuntimeTransition,
    policy: Option<&crate::AgentPolicyDecision>,
    aggregate: &LockedAgentAggregate,
    source: &LockedAgentCommand,
) -> Result<(), StoreError> {
    if aggregate.task_principal != principal.as_str() {
        return Err(StoreError::PrincipalMismatch);
    }
    if aggregate.goal_phase == GoalPhase::Complete {
        return Err(StoreError::GoalAlreadyTerminal);
    }
    if aggregate.task_lifecycle != TaskLifecycle::Running
        || aggregate.operational_status == OperationalStatus::Finished
    {
        return Err(StoreError::AgentTaskAlreadyTerminal);
    }
    if aggregate.goal_phase != GoalPhase::Active {
        return Err(StoreError::AgentTransitionInvalid(
            "goal transition requires an active goal".to_owned(),
        ));
    }
    if transition.expected_task_revision != aggregate.task_revision {
        return Err(StoreError::TaskRevisionConflict);
    }
    if transition.expected_goal_revision != aggregate.goal_revision {
        return Err(StoreError::GoalRevisionConflict);
    }
    if source.view.state != AgentCommandState::Claimed
        || source.view.revision != completion.expected_command_revision
    {
        return Err(StoreError::RevisionConflict);
    }
    if completion.event.command_key() != Some(&source.view.key)
        || transition.idempotency_key != format!("event:{}", completion.event.event_id())
    {
        return Err(StoreError::AgentTransitionInvalid(
            "completion event does not match its source command".to_owned(),
        ));
    }
    match (authority_command(transition), policy) {
        (Some(_), None) => {
            return Err(StoreError::AgentTransitionInvalid(
                "goal transition requires an allowed policy decision".to_owned(),
            ));
        }
        (None, Some(_)) => {
            return Err(StoreError::AgentTransitionInvalid(
                "external transition cannot carry a goal policy decision".to_owned(),
            ));
        }
        (Some(_), Some(policy)) => {
            if !policy.allowed {
                return Err(StoreError::AgentTransitionInvalid(
                    "current policy denied goal transition".to_owned(),
                ));
            }
            if policy.policy_revision != aggregate.session_policy_revision {
                return Err(StoreError::PolicyRevisionConflict);
            }
            if !valid_reason_code(&policy.reason_code) {
                return Err(StoreError::AgentTransitionInvalid(
                    "policy reason code is invalid".to_owned(),
                ));
            }
        }
        (None, None) => {}
    }
    validate_authority_shape(transition)?;
    Ok(())
}

fn validate_authority_shape(transition: &RuntimeTransition) -> Result<(), StoreError> {
    let sole_terminating_authority = matches!(
        transition.commands.as_slice(),
        [KernelCommand {
            payload: KernelCommandPayload::CompleteGoal(_) | KernelCommandPayload::FailTask(_),
            ..
        }]
    );
    if transition.snapshot.disposition == RuntimeDisposition::Terminating {
        if !sole_terminating_authority {
            return Err(StoreError::AgentTransitionInvalid(
                "terminating transition must propose exactly one completion or failure".to_owned(),
            ));
        }
    } else if transition.snapshot.disposition == RuntimeDisposition::Blocked {
        if !matches!(
            transition.commands.as_slice(),
            [KernelCommand {
                payload: KernelCommandPayload::BlockGoal(_),
                ..
            }]
        ) {
            return Err(StoreError::AgentTransitionInvalid(
                "blocked transition must propose exactly one BlockGoal".to_owned(),
            ));
        }
    } else if transition.commands.iter().any(|command| {
        matches!(
            command.payload,
            KernelCommandPayload::CompleteGoal(_)
                | KernelCommandPayload::BlockGoal(_)
                | KernelCommandPayload::FailTask(_)
        )
    }) {
        return Err(StoreError::AgentTransitionInvalid(
            "authority command disposition is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn authority_command(transition: &RuntimeTransition) -> Option<&KernelCommandPayload> {
    match transition.commands.as_slice() {
        [KernelCommand { payload, .. }]
            if matches!(
                payload,
                KernelCommandPayload::CompleteGoal(_) | KernelCommandPayload::BlockGoal(_)
            ) =>
        {
            Some(payload)
        }
        _ => None,
    }
}

fn authority_evidence_ids(command: Option<&KernelCommandPayload>) -> BTreeSet<EvidenceId> {
    match command {
        Some(KernelCommandPayload::CompleteGoal(command)) => command
            .assertions
            .iter()
            .map(|assertion| assertion.evidence_id.clone())
            .collect(),
        Some(KernelCommandPayload::BlockGoal(command)) => {
            command.evidence_ids.iter().cloned().collect()
        }
        _ => BTreeSet::new(),
    }
}

fn prospective_harness_evidence(
    completion: &CompleteAgentCommand,
) -> Result<Option<EvidenceRecord>, StoreError> {
    let RuntimeEvent::HarnessResult {
        outcome,
        fields,
        evidence_id,
        history,
        ..
    } = &completion.event
    else {
        return Ok(None);
    };
    Ok(Some(EvidenceRecord {
        id: evidence_id.clone(),
        task_id: completion.task_id.clone(),
        evidence_type: "harness.invocation_result".to_owned(),
        source: EvidenceSourceIdentity::BuiltinHarness {
            command_id: completion.command_id.clone(),
        },
        digest: canonical_sha256(&json!({
            "outcome": outcome,
            "fields": fields,
            "history": history,
        }))
        .map_err(|error| StoreError::InvalidData(error.to_string()))?,
    }))
}

async fn load_authorized_evidence(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    ids: &BTreeSet<EvidenceId>,
) -> Result<BTreeMap<EvidenceId, AuthorizedEvidence>, StoreError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let ids = ids
        .iter()
        .map(|id| *id.as_uuid())
        .collect::<Vec<uuid::Uuid>>();
    let rows = sqlx::query(
        "SELECT e.id, e.evidence_type, e.source_kind, e.content_digest, \
                c.command_kind, c.state AS command_state, c.runtime_key, c.result \
         FROM goal_evidence e \
         JOIN agent_commands c ON c.task_id = e.task_id AND c.id = e.source_identity \
         WHERE e.task_id = $1 AND e.id = ANY($2) ORDER BY e.id FOR UPDATE OF e",
    )
    .bind(task_id.as_uuid())
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    rows.iter()
        .map(|row| {
            let id = EvidenceId::new(row.get("id"));
            let source_authorized = row.get::<String, _>("source_kind") == "builtin_harness"
                && row.get::<String, _>("command_kind") == "invoke_harness";
            let digest_current = source_authorized
                && row.get::<String, _>("command_state") == "completed"
                && stored_harness_evidence_is_current(
                    row.get::<Option<Value>, _>("result"),
                    &id,
                    &row.get::<String, _>("runtime_key"),
                    &row.get::<String, _>("content_digest"),
                );
            Ok((
                id,
                AuthorizedEvidence {
                    evidence_type: row.get("evidence_type"),
                    source_authorized,
                    digest_current,
                },
            ))
        })
        .collect()
}

fn stored_harness_evidence_is_current(
    result: Option<Value>,
    expected_evidence_id: &EvidenceId,
    expected_command_key: &str,
    expected_digest: &str,
) -> bool {
    let Some(result) = result else {
        return false;
    };
    let Ok(RuntimeEvent::HarnessResult {
        command_key,
        outcome,
        fields,
        evidence_id,
        history,
        ..
    }) = serde_json::from_value(result)
    else {
        return false;
    };
    evidence_id == *expected_evidence_id
        && command_key.as_str() == expected_command_key
        && canonical_sha256(&json!({
            "outcome": outcome,
            "fields": fields,
            "history": history,
        }))
        .is_ok_and(|digest| digest == expected_digest)
}

fn validate_complete_goal(
    goal: &GoalContract,
    command: &CompleteGoal,
    evidence: &BTreeMap<EvidenceId, AuthorizedEvidence>,
) -> Result<(), StoreError> {
    let expected = goal
        .success
        .iter()
        .map(|criterion| (criterion.id.as_str(), criterion.evidence.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeMap::new();
    let mut distinct_evidence = HashSet::new();
    for assertion in &command.assertions {
        if actual
            .insert(assertion.criterion_id.as_str(), &assertion.evidence_id)
            .is_some()
            || !distinct_evidence.insert(&assertion.evidence_id)
        {
            return Err(StoreError::GoalEvidenceIncomplete);
        }
    }
    if actual.len() != expected.len() || actual.keys().ne(expected.keys()) {
        return Err(StoreError::GoalEvidenceIncomplete);
    }
    for (criterion_id, evidence_id) in actual {
        let record = evidence
            .get(evidence_id)
            .ok_or(StoreError::GoalEvidenceNotFound)?;
        if record.evidence_type != expected[criterion_id] {
            return Err(StoreError::GoalEvidenceTypeMismatch);
        }
        if !record.source_authorized {
            return Err(StoreError::GoalEvidenceUnauthorized);
        }
        if !record.digest_current {
            return Err(StoreError::GoalEvidenceStale);
        }
    }
    Ok(())
}

fn validate_block_goal(
    goal: &GoalContract,
    command: &BlockGoal,
    evidence: &BTreeMap<EvidenceId, AuthorizedEvidence>,
) -> Result<(), StoreError> {
    if usize::from(command.blocked_when_index) >= goal.blocked_when.len() {
        return Err(StoreError::AgentTransitionInvalid(
            "goal blocker index is outside the accepted goal".to_owned(),
        ));
    }
    if !valid_reason_code(&command.reason_code) {
        return Err(StoreError::AgentTransitionInvalid(
            "goal blocker reason code is invalid".to_owned(),
        ));
    }
    if !bounded_action_text(&command.explanation) {
        return Err(StoreError::AgentTransitionInvalid(
            "goal blocker explanation is invalid".to_owned(),
        ));
    }
    if !(1..=32).contains(&command.evidence_ids.len())
        || command.evidence_ids.iter().collect::<HashSet<_>>().len() != command.evidence_ids.len()
    {
        return Err(StoreError::GoalEvidenceIncomplete);
    }
    if !(1..=32).contains(&command.unlock_conditions.len())
        || command
            .unlock_conditions
            .iter()
            .any(|condition| !bounded_action_text(condition))
    {
        return Err(StoreError::AgentTransitionInvalid(
            "goal blocker unlock conditions are invalid".to_owned(),
        ));
    }
    for evidence_id in &command.evidence_ids {
        let record = evidence
            .get(evidence_id)
            .ok_or(StoreError::GoalEvidenceNotFound)?;
        if !record.source_authorized {
            return Err(StoreError::GoalEvidenceUnauthorized);
        }
        if !record.digest_current {
            return Err(StoreError::GoalEvidenceStale);
        }
    }
    Ok(())
}

fn bounded_action_text(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 2_048
}

fn reconcile_transition_commands(
    current_snapshot: &RuntimeStateSnapshot,
    event: &RuntimeEvent,
    transition: &RuntimeTransition,
    source: &LockedAgentCommand,
    persisted: &[LockedAgentCommand],
    current_task_revision: u64,
    ceilings: &RuntimeCeilings,
) -> Result<(), StoreError> {
    let replacement_task_revision = current_task_revision
        .checked_add(1)
        .ok_or_else(|| StoreError::InvalidData("user task revision is malformed".to_owned()))?;
    let current_state = KiteAgentRuntime::new(*ceilings)
        .decode_snapshot_for_task_revision(current_snapshot, current_task_revision)
        .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
    let replacement_state = KiteAgentRuntime::new(*ceilings)
        .decode_snapshot_for_task_revision(&transition.snapshot, replacement_task_revision)
        .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    let current_waits = runtime_waits(&current_state)?;
    let replacement_waits = runtime_waits(&replacement_state)?;
    let source_kind = external_command_kind(&source.view.payload).ok_or_else(|| {
        StoreError::AgentTransitionInvalid("source command is not external work".to_owned())
    })?;
    if current_waits.get(source.view.key.as_str()) != Some(&source_kind) {
        return Err(StoreError::AgentTransitionInvalid(
            "source command does not match the current snapshot wait".to_owned(),
        ));
    }
    let event_kind = event_command_kind(event, source_kind)?;
    if event_kind != source_kind {
        return Err(StoreError::AgentTransitionInvalid(
            "event kind does not match persisted command".to_owned(),
        ));
    }
    if replacement_waits.contains_key(source.view.key.as_str()) {
        return Err(StoreError::AgentTransitionInvalid(
            "consumed source wait remains in replacement snapshot".to_owned(),
        ));
    }

    let mut new_keys = BTreeSet::new();
    let mut new_external = BTreeMap::new();
    for command in &transition.commands {
        if persisted
            .iter()
            .any(|stored| stored.view.key == command.key)
            || !new_keys.insert(command.key.as_str())
        {
            return Err(StoreError::AgentTransitionInvalid(
                "new command runtime key is duplicated".to_owned(),
            ));
        }
        if let Some(kind) = external_command_kind(&command.payload) {
            new_external.insert(command.key.as_str(), kind);
            if replacement_waits.get(command.key.as_str()) != Some(&kind) {
                return Err(StoreError::AgentTransitionInvalid(
                    "new command does not have exactly one matching snapshot wait".to_owned(),
                ));
            }
        }
    }
    let outstanding = persisted
        .iter()
        .filter(|stored| stored.view.id != source.view.id)
        .filter(|stored| {
            matches!(
                stored.view.state,
                AgentCommandState::Pending | AgentCommandState::Claimed
            )
        })
        .map(|stored| {
            external_command_kind(&stored.view.payload)
                .map(|kind| (stored.view.key.as_str(), kind))
                .ok_or_else(|| {
                    StoreError::AgentTransitionInvalid(
                        "outstanding authority command cannot be dispatched".to_owned(),
                    )
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    for (key, kind) in &replacement_waits {
        let matches_new = new_external.get(key.as_str()) == Some(kind);
        let matches_persisted = outstanding.get(key.as_str()) == Some(kind);
        if matches_new == matches_persisted {
            return Err(StoreError::AgentTransitionInvalid(
                "replacement snapshot wait does not match exactly one command".to_owned(),
            ));
        }
    }
    for (key, kind) in &outstanding {
        if replacement_waits.get(*key) != Some(kind) {
            return Err(StoreError::AgentTransitionInvalid(
                "outstanding command is absent from replacement snapshot".to_owned(),
            ));
        }
    }
    if transition.snapshot.disposition == RuntimeDisposition::Running
        && transition.commands.is_empty()
        && !structured_partial_join(
            &current_state,
            &replacement_state,
            &source.view.key,
            &outstanding,
        )
    {
        return Err(StoreError::AgentTransitionInvalid(
            "running transition without new commands is not a structured partial join".to_owned(),
        ));
    }
    Ok(())
}

fn event_command_kind(
    event: &RuntimeEvent,
    source_kind: &'static str,
) -> Result<&'static str, StoreError> {
    match event {
        RuntimeEvent::HarnessResult { .. } => Ok("invoke_harness"),
        RuntimeEvent::ContextResolved { .. } => Ok("request_context"),
        RuntimeEvent::SemanticEvent { .. } => Ok("wait_for_event"),
        RuntimeEvent::ApprovalResult { .. } => Ok("request_approval"),
        RuntimeEvent::CommandFailed { .. } => Ok(source_kind),
        RuntimeEvent::PrincipalResumed { .. } => Err(StoreError::AgentTransitionInvalid(
            "event kind does not name an external command".to_owned(),
        )),
    }
}

fn runtime_waits(state: &KiteRuntimeState) -> Result<BTreeMap<String, &'static str>, StoreError> {
    let mut waits = BTreeMap::new();
    for frame in state.frames.values() {
        let waiting = match &frame.state {
            KiteFrameState::WaitingForHarness { command_key } => {
                Some((command_key, "invoke_harness"))
            }
            KiteFrameState::WaitingForApproval { command_key, .. } => {
                Some((command_key, "request_approval"))
            }
            _ => None,
        };
        if let Some((key, kind)) = waiting
            && waits.insert(key.as_str().to_owned(), kind).is_some()
        {
            return Err(StoreError::AgentTransitionInvalid(
                "snapshot wait key is duplicated".to_owned(),
            ));
        }
    }
    Ok(waits)
}

fn record_recovered_wait(
    waits: &BTreeMap<String, &'static str>,
    command_waits: &mut BTreeMap<String, &'static str>,
    command: &LockedAgentCommand,
) -> Result<(), StoreError> {
    let kind = external_command_kind(&command.view.payload).ok_or_else(|| {
        StoreError::RuntimeStateInvalid(
            "recovered outstanding command is not external work".to_owned(),
        )
    })?;
    if waits.get(command.view.key.as_str()) != Some(&kind)
        || command_waits
            .insert(command.view.key.as_str().to_owned(), kind)
            .is_some()
    {
        return Err(StoreError::RuntimeStateInvalid(
            "recovered command does not match its snapshot wait".to_owned(),
        ));
    }
    Ok(())
}

fn structured_partial_join(
    before: &KiteRuntimeState,
    after: &KiteRuntimeState,
    source_key: &RuntimeCommandKey,
    outstanding: &BTreeMap<&str, &'static str>,
) -> bool {
    let source = before.frames.values().find(|frame| {
        matches!(
            &frame.state,
            KiteFrameState::WaitingForHarness { command_key }
                | KiteFrameState::WaitingForApproval { command_key, .. }
                if command_key == source_key
        )
    });
    let Some(source) = source else {
        return false;
    };
    let Some(parent_id) = source.parent_id else {
        return false;
    };
    let Some(parent) = before.frames.get(&parent_id) else {
        return false;
    };
    let KiteFrameState::WaitingForChildren { child_ids } = &parent.state else {
        return false;
    };
    if child_ids.len() < 2 || !child_ids.contains(&source.id) {
        return false;
    }
    if !matches!(
        after.frames.get(&source.id).map(|frame| &frame.state),
        Some(KiteFrameState::Returned { .. })
    ) || !matches!(
        after.frames.get(&parent_id).map(|frame| &frame.state),
        Some(KiteFrameState::WaitingForChildren { .. })
    ) {
        return false;
    }
    child_ids.iter().filter(|id| **id != source.id).any(|id| {
        after
            .frames
            .get(id)
            .is_some_and(|frame| match &frame.state {
                KiteFrameState::WaitingForHarness { command_key }
                | KiteFrameState::WaitingForApproval { command_key, .. } => {
                    outstanding.contains_key(command_key.as_str())
                }
                _ => false,
            })
    })
}

fn external_command_kind(payload: &KernelCommandPayload) -> Option<&'static str> {
    match payload {
        KernelCommandPayload::RequestContext(_) => Some("request_context"),
        KernelCommandPayload::WaitForEvent(_) => Some("wait_for_event"),
        KernelCommandPayload::InvokeHarness(_) => Some("invoke_harness"),
        KernelCommandPayload::RequestApproval(_) => Some("request_approval"),
        KernelCommandPayload::CompleteGoal(_)
        | KernelCommandPayload::BlockGoal(_)
        | KernelCommandPayload::FailTask(_) => None,
    }
}

fn transition_task_state(
    transition: &RuntimeTransition,
    ceilings: &RuntimeCeilings,
) -> Result<(&'static str, OperationalStatus, Option<Value>), StoreError> {
    match transition.snapshot.disposition {
        RuntimeDisposition::Running => {
            let state = KiteAgentRuntime::new(*ceilings)
                .decode_snapshot(&transition.snapshot)
                .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
            let waits = runtime_waits(&state)?;
            let mut kinds = waits.values();
            let first = kinds.next().ok_or_else(|| {
                StoreError::AgentTransitionInvalid("running snapshot has no wait".to_owned())
            })?;
            if kinds.any(|kind| kind != first) {
                return Err(StoreError::AgentTransitionInvalid(
                    "running snapshot has mixed external command kinds".to_owned(),
                ));
            }
            let status = match *first {
                "invoke_harness" => OperationalStatus::WaitingForHarness,
                "request_approval" => OperationalStatus::WaitingForApproval,
                "request_context" => OperationalStatus::WaitingForContext,
                "wait_for_event" => OperationalStatus::WaitingForEvent,
                _ => unreachable!("runtime_waits returns closed command kinds"),
            };
            Ok(("running", status, None))
        }
        RuntimeDisposition::Returned if transition.commands.is_empty() => {
            Ok(("succeeded", OperationalStatus::Finished, None))
        }
        RuntimeDisposition::Terminating => match transition.commands.as_slice() {
            [
                KernelCommand {
                    payload: KernelCommandPayload::CompleteGoal(_),
                    ..
                },
            ] => Ok(("succeeded", OperationalStatus::Finished, None)),
            [
                KernelCommand {
                    payload: KernelCommandPayload::FailTask(failure),
                    ..
                },
            ] => Ok((
                "failed",
                OperationalStatus::Finished,
                Some(json!({"code": failure.code, "message": failure.message})),
            )),
            _ => Err(StoreError::AgentTransitionInvalid(
                "terminating transition command is invalid".to_owned(),
            )),
        },
        RuntimeDisposition::Blocked
            if matches!(
                transition.commands.as_slice(),
                [KernelCommand {
                    payload: KernelCommandPayload::BlockGoal(_),
                    ..
                }]
            ) =>
        {
            Ok(("running", OperationalStatus::Blocked, None))
        }
        _ => Err(StoreError::AgentTransitionInvalid(
            "runtime transition disposition is invalid".to_owned(),
        )),
    }
}

fn command_integrity_error() -> StoreError {
    StoreError::RuntimeStateInvalid("stored command integrity check failed".to_owned())
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

fn pause_request_digest(request: &PauseUserTask) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "sessionId": &request.session_id,
        "taskId": &request.task_id,
        "expectedTaskRevision": request.expected_task_revision,
        "expectedGoalRevision": request.expected_goal_revision,
        "reason": &request.reason,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn resume_request_digest(request: &ResumeUserTask) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "sessionId": &request.session_id,
        "taskId": &request.task_id,
        "expectedTaskRevision": request.expected_task_revision,
        "expectedGoalRevision": request.expected_goal_revision,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn cancel_request_digest(request: &CancelUserTask) -> Result<String, StoreError> {
    canonical_sha256(&json!({
        "sessionId": &request.session_id,
        "taskId": &request.task_id,
        "expectedTaskRevision": request.expected_task_revision,
        "expectedGoalRevision": request.expected_goal_revision,
        "reason": &request.reason,
    }))
    .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))
}

fn validate_principal_action_revisions(
    aggregate: &LockedAgentAggregate,
    expected_task_revision: &u64,
    expected_goal_revision: &u64,
) -> Result<(), StoreError> {
    if aggregate.task_revision != *expected_task_revision {
        return Err(StoreError::TaskRevisionConflict);
    }
    if aggregate.goal_revision != *expected_goal_revision {
        return Err(StoreError::GoalRevisionConflict);
    }
    Ok(())
}

fn next_revision(current: u64, name: &str) -> Result<u64, StoreError> {
    current
        .checked_add(1)
        .ok_or_else(|| StoreError::InvalidData(format!("{name} is malformed")))
}

async fn lock_optional_runtime_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    task_revision: u64,
    ceilings: &RuntimeCeilings,
) -> Result<Option<RuntimeStateSnapshot>, StoreError> {
    let row = sqlx::query(
        "SELECT task_revision, schema_version, runtime_id, disposition, payload, \
                ceiling_bytes, canonical_bytes, snapshot_digest \
         FROM agent_runtime_snapshots WHERE task_id = $1 FOR UPDATE",
    )
    .bind(task_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref()
        .map(|row| runtime_snapshot_from_locked_row(row, task_revision, ceilings))
        .transpose()
}

async fn advance_runtime_snapshot_revision(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    task_revision: u64,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE agent_runtime_snapshots SET task_revision = $1, \
         updated_at = CURRENT_TIMESTAMP WHERE task_id = $2",
    )
    .bind(to_i64(task_revision, "user task revision is malformed")?)
    .bind(task_id.as_uuid())
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(snapshot_integrity_error());
    }
    Ok(())
}

async fn replace_runtime_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    task_revision: u64,
    snapshot: &RuntimeStateSnapshot,
    canonical_bytes: usize,
    snapshot_digest: &str,
) -> Result<(), StoreError> {
    let updated = sqlx::query(
        "UPDATE agent_runtime_snapshots SET task_revision = $1, schema_version = $2, \
         runtime_id = $3, disposition = $4, payload = $5, ceiling_bytes = $6, \
         canonical_bytes = $7, snapshot_digest = $8, updated_at = CURRENT_TIMESTAMP \
         WHERE task_id = $9",
    )
    .bind(to_i64(task_revision, "user task revision is malformed")?)
    .bind(i32::from(snapshot.schema_version))
    .bind(&snapshot.runtime_id)
    .bind(runtime_disposition_name(snapshot.disposition))
    .bind(&snapshot.payload)
    .bind(i32::try_from(snapshot.ceiling_bytes).map_err(|_| snapshot_integrity_error())?)
    .bind(i32::try_from(canonical_bytes).map_err(|_| snapshot_integrity_error())?)
    .bind(snapshot_digest)
    .bind(task_id.as_uuid())
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(snapshot_integrity_error());
    }
    Ok(())
}

async fn insert_prepared_commands(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    transition_id: uuid::Uuid,
    commands: &[PreparedCommand<'_>],
) -> Result<(), StoreError> {
    for (ordinal, prepared) in commands.iter().enumerate() {
        sqlx::query(
            "INSERT INTO agent_commands \
             (id, task_id, transition_id, ordinal, schema_version, runtime_key, \
              command_kind, payload, payload_digest) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(task_id.as_uuid())
        .bind(transition_id)
        .bind(i32::try_from(ordinal).map_err(|_| {
            StoreError::AgentTransitionInvalid("command ordinal is invalid".to_owned())
        })?)
        .bind(i32::from(prepared.command.schema_version))
        .bind(prepared.command.key.as_str())
        .bind(command_kind(&prepared.command.payload))
        .bind(&prepared.payload)
        .bind(&prepared.payload_digest)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn insert_agent_action(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
    action_kind: &str,
    idempotency_key: &str,
    request_digest: &str,
    result_task_revision: u64,
    result_goal_revision: u64,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO agent_task_actions \
         (id, task_id, action_kind, idempotency_key, request_digest, \
          result_task_revision, result_goal_revision) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(task_id.as_uuid())
    .bind(action_kind)
    .bind(idempotency_key)
    .bind(request_digest)
    .bind(to_i64(
        result_task_revision,
        "user task revision is malformed",
    )?)
    .bind(to_i64(result_goal_revision, "goal revision is malformed")?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn outstanding_operational_status(
    commands: &[LockedAgentCommand],
) -> Result<OperationalStatus, StoreError> {
    let outstanding = commands
        .iter()
        .filter(|command| {
            matches!(
                command.view.state,
                AgentCommandState::Pending | AgentCommandState::Claimed
            )
        })
        .collect::<Vec<_>>();
    let Some(first) = outstanding.first() else {
        return Err(StoreError::AgentTransitionInvalid(
            "resumed execution has no outstanding command".to_owned(),
        ));
    };
    let status = external_command_status(&first.view.payload)?;
    for command in outstanding.iter().skip(1) {
        if external_command_status(&command.view.payload)? != status {
            return Err(StoreError::AgentTransitionInvalid(
                "resumed execution has mixed command kinds".to_owned(),
            ));
        }
    }
    Ok(status)
}

fn validate_blocked_resume_transition(
    request: &ResumeUserTask,
    aggregate: &LockedAgentAggregate,
    current_snapshot: &RuntimeStateSnapshot,
    commands: &[LockedAgentCommand],
    transition: &RuntimeTransition,
    ceilings: &RuntimeCeilings,
) -> Result<(), StoreError> {
    if !goal_edge_allowed(GoalPhase::Blocked, GoalPhase::Active)
        || aggregate.operational_status != OperationalStatus::Blocked
        || aggregate.current_blocker_id.is_none()
        || current_snapshot.disposition != RuntimeDisposition::Blocked
    {
        return Err(StoreError::AgentTransitionInvalid(
            "blocked resume state is invalid".to_owned(),
        ));
    }
    if transition.expected_task_revision != aggregate.task_revision {
        return Err(StoreError::TaskRevisionConflict);
    }
    if transition.expected_goal_revision != aggregate.goal_revision {
        return Err(StoreError::GoalRevisionConflict);
    }
    if transition.idempotency_key
        != format!(
            "event:{}",
            principal_resumed_event_id(&request.task_id, &request.idempotency_key)
        )
        || transition.snapshot.disposition != RuntimeDisposition::Running
        || transition.commands.is_empty()
        || transition.commands.len() > ceilings.max_commands_per_transition
    {
        return Err(StoreError::AgentTransitionInvalid(
            "blocked resume transition envelope is invalid".to_owned(),
        ));
    }
    let current_state = KiteAgentRuntime::new(*ceilings)
        .decode_snapshot_for_task_revision(current_snapshot, aggregate.task_revision)
        .map_err(|error| StoreError::RuntimeStateInvalid(error.to_string()))?;
    let replacement_revision = next_revision(aggregate.task_revision, "user task revision")?;
    let replacement_state = KiteAgentRuntime::new(*ceilings)
        .decode_snapshot_for_task_revision(&transition.snapshot, replacement_revision)
        .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
    let current_waits = runtime_waits(&current_state)?;
    let replacement_waits = runtime_waits(&replacement_state)?;
    let outstanding = commands
        .iter()
        .filter(|command| {
            matches!(
                command.view.state,
                AgentCommandState::Pending | AgentCommandState::Claimed
            )
        })
        .map(|command| {
            external_command_kind(&command.view.payload)
                .map(|kind| (command.view.key.as_str(), kind))
                .ok_or_else(|| {
                    StoreError::AgentTransitionInvalid(
                        "blocked execution has an outstanding authority command".to_owned(),
                    )
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if current_waits.len() != outstanding.len()
        || current_waits
            .iter()
            .any(|(key, kind)| outstanding.get(key.as_str()) != Some(kind))
    {
        return Err(StoreError::AgentTransitionInvalid(
            "blocked snapshot waits do not match outstanding commands".to_owned(),
        ));
    }
    let mut new_commands = BTreeMap::new();
    for command in &transition.commands {
        if command.schema_version != 1
            || commands.iter().any(|stored| stored.view.key == command.key)
            || new_commands.contains_key(command.key.as_str())
        {
            return Err(StoreError::AgentTransitionInvalid(
                "resumed command key is invalid or duplicated".to_owned(),
            ));
        }
        let kind = external_command_kind(&command.payload).ok_or_else(|| {
            StoreError::AgentTransitionInvalid(
                "blocked resume can propose only external work".to_owned(),
            )
        })?;
        if replacement_waits.get(command.key.as_str()) != Some(&kind) {
            return Err(StoreError::AgentTransitionInvalid(
                "resumed command does not match the replacement wait".to_owned(),
            ));
        }
        new_commands.insert(command.key.as_str(), kind);
    }
    for (key, kind) in &replacement_waits {
        let old_match = outstanding.get(key.as_str()) == Some(kind);
        let new_match = new_commands.get(key.as_str()) == Some(kind);
        if old_match == new_match {
            return Err(StoreError::AgentTransitionInvalid(
                "replacement wait does not match exactly one resumed command".to_owned(),
            ));
        }
    }
    if outstanding
        .iter()
        .any(|(key, kind)| replacement_waits.get(*key) != Some(kind))
    {
        return Err(StoreError::AgentTransitionInvalid(
            "blocked resume dropped outstanding work".to_owned(),
        ));
    }
    Ok(())
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

const fn goal_phase_name(phase: GoalPhase) -> &'static str {
    match phase {
        GoalPhase::Active => "active",
        GoalPhase::Paused => "paused",
        GoalPhase::Blocked => "blocked",
        GoalPhase::Complete => "complete",
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
