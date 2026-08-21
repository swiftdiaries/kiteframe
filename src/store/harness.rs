use serde_json::Value;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

use crate::harness::{
    HarnessEventEnvelope, HarnessInvocationId, HarnessProfileId, HarnessTransport,
    canonical_outcome_unknown_terminal_payload, terminal_idempotency_key,
};
use crate::{AgentCommandId, RuntimeEvent, UserTaskId};

use super::{
    HarnessInvocationEventRecord, HarnessInvocationState, HarnessInvocationView,
    HarnessPluginRegistration, PostgresStore, PrepareHarnessInvocation, StoreError,
    models::harness_event_from_value,
};

impl PostgresStore {
    pub async fn append_harness_event_envelope(
        &self,
        event: &HarnessEventEnvelope,
    ) -> Result<(), StoreError> {
        event
            .validate()
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let record = HarnessInvocationEventRecord::new(
            event.invocation_id.clone(),
            event.sequence,
            event.event.clone(),
        )?;
        if record.event_digest != event.event_digest {
            return Err(StoreError::InvalidData(
                "harness envelope digest is incompatible with the journal".to_owned(),
            ));
        }
        self.append_harness_invocation_event(&record).await
    }

    pub async fn register_harness_plugin(
        &self,
        registration: HarnessPluginRegistration,
    ) -> Result<HarnessPluginRegistration, StoreError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO harness_plugin_registrations \
             (profile_id, protocol_version, profile_digest, source_repository, source_revision, transport, enabled) \
             VALUES ($1, 1, $2, $3, $4, $5, $6) \
             ON CONFLICT (profile_id) DO NOTHING",
        )
        .bind(registration.profile_id.as_str())
        .bind(&registration.profile_digest)
        .bind(&registration.source_repository)
        .bind(&registration.source_revision)
        .bind(transport_name(registration.transport))
        .bind(registration.enabled)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(
            "SELECT profile_id, profile_digest, source_repository, source_revision, transport, enabled \
             FROM harness_plugin_registrations WHERE profile_id = $1 FOR UPDATE",
        )
        .bind(registration.profile_id.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let stored = registration_from_row(&row)?;
        if stored != registration {
            return Err(StoreError::IdempotencyConflict);
        }
        tx.commit().await?;
        Ok(stored)
    }

    pub async fn harness_plugin_transport(
        &self,
        profile_id: &HarnessProfileId,
    ) -> Result<HarnessTransport, StoreError> {
        let transport: String = sqlx::query_scalar(
            "SELECT transport FROM harness_plugin_registrations WHERE profile_id = $1",
        )
        .bind(profile_id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| {
            StoreError::InvalidData("harness plugin registration is absent".to_owned())
        })?;
        transport_from_row(&transport)
    }

    pub async fn harness_invocation_for_command_revision(
        &self,
        command_id: &AgentCommandId,
        command_revision: u64,
    ) -> Result<Option<HarnessInvocationView>, StoreError> {
        let command_revision =
            harness_i64(command_revision, "harness command revision is malformed")?;
        let row = sqlx::query(
            "SELECT invocations.invocation_id, invocations.command_id, invocations.command_revision, \
                    invocations.profile_id, invocations.profile_digest, invocations.payload_digest, \
                    invocations.state, invocations.attempt, invocations.prompt_accepted, \
                    invocations.terminal, invocations.terminal_request_digest, invocations.terminal_payload, \
                    registrations.profile_digest AS registered_profile_digest \
             FROM harness_invocations invocations \
             JOIN harness_plugin_registrations registrations USING (profile_id) \
             WHERE invocations.command_id = $1 AND invocations.command_revision = $2",
        )
        .bind(command_id.as_uuid())
        .bind(command_revision)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let view = harness_invocation_from_row(&row)?;
        if view.profile_digest != row.get::<String, _>("registered_profile_digest") {
            return Err(StoreError::IdempotencyConflict);
        }
        Ok(Some(view))
    }

    pub async fn prepare_harness_invocation(
        &self,
        request: PrepareHarnessInvocation<'_>,
    ) -> Result<HarnessInvocationView, StoreError> {
        let command_revision = harness_i64(
            request.command_revision,
            "harness command revision is malformed",
        )?;
        let mut tx = self.pool().begin().await?;
        let command_state: String = sqlx::query_scalar(
            "SELECT state FROM agent_commands WHERE id = $1 AND revision = $2 FOR UPDATE",
        )
        .bind(request.command_id.as_uuid())
        .bind(command_revision)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::RevisionConflict)?;
        let registration = sqlx::query(
            "SELECT profile_digest, enabled FROM harness_plugin_registrations \
             WHERE profile_id = $1 FOR UPDATE",
        )
        .bind(request.profile_id.as_str())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            StoreError::InvalidData("harness plugin registration is absent".to_owned())
        })?;
        if registration.get::<String, _>("profile_digest") != request.profile_digest
            || !registration.get::<bool, _>("enabled")
        {
            return Err(StoreError::IdempotencyConflict);
        }
        if let Some(row) = sqlx::query(
            "SELECT invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
                    state, attempt, prompt_accepted, terminal, terminal_request_digest, terminal_payload \
             FROM harness_invocations WHERE invocation_id = $1 FOR UPDATE",
        )
        .bind(request.invocation_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        {
            let view = harness_invocation_from_row(&row)?;
            validate_invocation_identity_replay(&view, &request)?;
            tx.commit().await?;
            return Ok(view);
        }
        if command_state != "claimed" {
            return Err(StoreError::RevisionConflict);
        }
        let insert = sqlx::query(
            "INSERT INTO harness_invocations \
             (invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
              state, attempt, prompt_accepted) \
             VALUES ($1, $2, $3, $4, $5, $6, 'prepared', 0, FALSE) \
             ON CONFLICT (command_id, command_revision) DO NOTHING",
        )
        .bind(request.invocation_id.as_uuid())
        .bind(request.command_id.as_uuid())
        .bind(command_revision)
        .bind(request.profile_id.as_str())
        .bind(request.profile_digest)
        .bind(request.payload_digest)
        .execute(&mut *tx)
        .await;
        if let Err(error) = insert {
            if is_harness_invocation_identity_conflict(&error) {
                return Err(StoreError::IdempotencyConflict);
            }
            return Err(error.into());
        }
        let row = sqlx::query(
            "SELECT invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
                    state, attempt, prompt_accepted, terminal, terminal_request_digest, terminal_payload \
             FROM harness_invocations WHERE command_id = $1 AND command_revision = $2 FOR UPDATE",
        )
        .bind(request.command_id.as_uuid())
        .bind(command_revision)
        .fetch_one(&mut *tx)
        .await?;
        let view = harness_invocation_from_row(&row)?;
        validate_command_revision_replay(&view, &request)?;
        tx.commit().await?;
        Ok(view)
    }

    pub async fn mark_harness_invocation_started(
        &self,
        invocation_id: &HarnessInvocationId,
        expected_attempt: u64,
    ) -> Result<HarnessInvocationView, StoreError> {
        let mut tx = self.pool().begin().await?;
        let (command_state, view) =
            lock_harness_invocation_with_command(&mut tx, invocation_id).await?;
        if command_state == "cancelled" {
            let view = settle_harness_cancelled(&mut tx, &view).await?;
            tx.commit().await?;
            return Ok(view);
        }
        if view.state == HarnessInvocationState::Started && view.attempt == expected_attempt {
            tx.commit().await?;
            return Ok(view);
        }
        if view.state != HarnessInvocationState::Prepared || view.attempt != expected_attempt {
            return Err(StoreError::RevisionConflict);
        }
        let view = update_invocation_state(
            &mut tx,
            invocation_id,
            HarnessInvocationState::Started,
            expected_attempt,
            false,
            false,
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(view)
    }

    pub async fn record_harness_retryable_failure(
        &self,
        invocation_id: &HarnessInvocationId,
        expected_attempt: u64,
        max_pre_accept_attempts: u64,
        request_digest: &str,
        terminal_payload: &Value,
    ) -> Result<HarnessInvocationView, StoreError> {
        if request_digest.is_empty() || !terminal_payload.is_object() {
            return Err(StoreError::InvalidData(
                "harness retryable failure request is malformed".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let (command_state, view) =
            lock_harness_invocation_with_command(&mut tx, invocation_id).await?;
        if command_state == "cancelled" {
            let view = settle_harness_cancelled(&mut tx, &view).await?;
            tx.commit().await?;
            return Ok(view);
        }
        if view.state == HarnessInvocationState::Failed && view.attempt == expected_attempt {
            if view.terminal_request_digest.as_deref() == Some(request_digest)
                && view.terminal_payload.as_ref() == Some(terminal_payload)
            {
                tx.commit().await?;
                return Ok(view);
            }
            return Err(StoreError::IdempotencyConflict);
        }
        if view.state != HarnessInvocationState::Started
            || view.prompt_accepted
            || view.attempt != expected_attempt
        {
            return Err(StoreError::RevisionConflict);
        }
        let (state, attempt, terminal, request_digest) =
            if expected_attempt < max_pre_accept_attempts {
                (
                    HarnessInvocationState::Started,
                    expected_attempt.checked_add(1).ok_or_else(|| {
                        StoreError::InvalidData("harness attempt is malformed".to_owned())
                    })?,
                    false,
                    None,
                )
            } else {
                (
                    HarnessInvocationState::Failed,
                    expected_attempt,
                    true,
                    Some(request_digest),
                )
            };
        let view = update_invocation_state(
            &mut tx,
            invocation_id,
            state,
            attempt,
            false,
            terminal,
            request_digest,
            terminal.then_some(terminal_payload),
        )
        .await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Commit this barrier immediately before the adapter writes the prompt bytes.
    pub async fn mark_harness_prompt_accepted(
        &self,
        invocation_id: &HarnessInvocationId,
        expected_attempt: u64,
    ) -> Result<HarnessInvocationView, StoreError> {
        let mut tx = self.pool().begin().await?;
        let (command_state, view) =
            lock_harness_invocation_with_command(&mut tx, invocation_id).await?;
        if command_state == "cancelled" {
            let view = settle_harness_cancelled(&mut tx, &view).await?;
            tx.commit().await?;
            return Ok(view);
        }
        if view.state == HarnessInvocationState::Accepted && view.attempt == expected_attempt {
            tx.commit().await?;
            return Ok(view);
        }
        if view.state != HarnessInvocationState::Started
            || view.prompt_accepted
            || view.attempt != expected_attempt
        {
            return Err(StoreError::RevisionConflict);
        }
        let view = update_invocation_state(
            &mut tx,
            invocation_id,
            HarnessInvocationState::Accepted,
            expected_attempt,
            true,
            false,
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(view)
    }

    pub async fn append_harness_invocation_event(
        &self,
        event: &HarnessInvocationEventRecord,
    ) -> Result<(), StoreError> {
        event.validate_digest()?;
        let sequence = harness_i64(event.sequence, "harness event sequence is malformed")?;
        let event_json = serde_json::to_value(&event.event)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let mut tx = self.pool().begin().await?;
        let _ = lock_harness_invocation(&mut tx, &event.invocation_id).await?;
        if let Some(existing) = sqlx::query(
            "SELECT invocation_id, sequence, event_digest, event FROM harness_invocation_events \
             WHERE invocation_id = $1 AND sequence = $2 FOR UPDATE",
        )
        .bind(event.invocation_id.as_uuid())
        .bind(sequence)
        .fetch_optional(&mut *tx)
        .await?
        {
            let stored = harness_event_record_from_row(&existing)?;
            if stored.event_digest == event.event_digest {
                tx.commit().await?;
                return Ok(());
            }
            return Err(StoreError::IdempotencyConflict);
        }
        let maximum: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), -1) FROM harness_invocation_events WHERE invocation_id = $1",
        )
        .bind(event.invocation_id.as_uuid())
        .fetch_one(&mut *tx)
        .await?;
        if sequence
            != maximum.checked_add(1).ok_or_else(|| {
                StoreError::InvalidData("harness event sequence is malformed".to_owned())
            })?
        {
            return Err(StoreError::RevisionConflict);
        }
        sqlx::query(
            "INSERT INTO harness_invocation_events \
             (invocation_id, sequence, event_digest, event) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (invocation_id, sequence) DO NOTHING",
        )
        .bind(event.invocation_id.as_uuid())
        .bind(sequence)
        .bind(&event.event_digest)
        .bind(event_json)
        .execute(&mut *tx)
        .await?;
        let stored = sqlx::query(
            "SELECT invocation_id, sequence, event_digest, event FROM harness_invocation_events \
             WHERE invocation_id = $1 AND sequence = $2 FOR UPDATE",
        )
        .bind(event.invocation_id.as_uuid())
        .bind(sequence)
        .fetch_one(&mut *tx)
        .await?;
        let stored = harness_event_record_from_row(&stored)?;
        if stored.event_digest != event.event_digest {
            return Err(StoreError::IdempotencyConflict);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn harness_invocation_events(
        &self,
        invocation_id: &HarnessInvocationId,
    ) -> Result<Vec<HarnessInvocationEventRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT invocation_id, sequence, event_digest, event FROM harness_invocation_events \
             WHERE invocation_id = $1 ORDER BY sequence",
        )
        .bind(invocation_id.as_uuid())
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(harness_event_record_from_row).collect()
    }

    pub async fn settle_harness_invocation(
        &self,
        invocation_id: &HarnessInvocationId,
        expected_attempt: u64,
        state: HarnessInvocationState,
        request_digest: &str,
        terminal_payload: &Value,
    ) -> Result<HarnessInvocationView, StoreError> {
        if !state.is_terminal() || request_digest.is_empty() || !terminal_payload.is_object() {
            return Err(StoreError::InvalidData(
                "harness terminal settlement is malformed".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let (command_state, view) =
            lock_harness_invocation_with_command(&mut tx, invocation_id).await?;
        if command_state == "cancelled" {
            let view = settle_harness_cancelled(&mut tx, &view).await?;
            tx.commit().await?;
            return Ok(view);
        }
        if !terminal_state_matches_command_winner(state, &command_state) {
            return Err(StoreError::RevisionConflict);
        }
        if view.terminal {
            if view.state == state
                && view.terminal_request_digest.as_deref() == Some(request_digest)
                && view.terminal_payload.as_ref() == Some(terminal_payload)
            {
                tx.commit().await?;
                return Ok(view);
            }
            return Err(StoreError::IdempotencyConflict);
        }
        if view.attempt != expected_attempt {
            return Err(StoreError::RevisionConflict);
        }
        let view = update_invocation_state(
            &mut tx,
            invocation_id,
            state,
            expected_attempt,
            view.prompt_accepted,
            true,
            Some(request_digest),
            Some(terminal_payload),
        )
        .await?;
        tx.commit().await?;
        Ok(view)
    }

    pub async fn recover_harness_invocations(
        &self,
        limit: i64,
    ) -> Result<Vec<HarnessInvocationView>, StoreError> {
        self.recover_harness_invocations_excluding(limit, &[]).await
    }

    pub async fn recover_harness_invocations_excluding(
        &self,
        limit: i64,
        excluded: &[HarnessInvocationId],
    ) -> Result<Vec<HarnessInvocationView>, StoreError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let excluded = excluded
            .iter()
            .map(HarnessInvocationId::as_uuid)
            .collect::<Vec<_>>();
        let candidates = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT invocations.invocation_id \
             FROM harness_invocations invocations \
             JOIN agent_commands commands ON commands.id = invocations.command_id \
             WHERE NOT (invocations.invocation_id = ANY($1)) AND ( \
                    (invocations.terminal = FALSE AND commands.state = 'cancelled') \
                 OR (invocations.terminal = FALSE AND commands.state = 'completed') \
                 OR (invocations.state = 'accepted' AND commands.state = 'claimed') \
                 OR (invocations.terminal = TRUE AND commands.completion_idempotency_key IS NULL) \
                 OR (commands.state = 'claimed' AND (invocations.state = 'prepared' \
                     OR (invocations.state = 'started' AND invocations.prompt_accepted = FALSE))) \
             ) \
             ORDER BY CASE \
                    WHEN invocations.terminal = FALSE AND commands.state = 'cancelled' THEN 0 \
                    WHEN invocations.terminal = FALSE AND commands.state = 'completed' THEN 1 \
                    WHEN invocations.state = 'accepted' THEN 2 \
                    WHEN invocations.terminal = TRUE THEN 3 \
                    ELSE 4 END, \
                    invocations.created_at, invocations.invocation_id \
             LIMIT 256",
        )
        .bind(excluded)
        .fetch_all(self.pool())
        .await?;
        let total_limit = usize::try_from(limit.min(256)).expect("positive recovery limit");
        let mut recovered = Vec::with_capacity(total_limit);
        for invocation_id in candidates {
            if let Some(view) = self
                .recover_one_harness_invocation(&HarnessInvocationId::new(invocation_id))
                .await?
            {
                recovered.push(view);
                if recovered.len() == total_limit {
                    break;
                }
            }
        }
        Ok(recovered)
    }

    pub async fn refresh_harness_recovery_invocation(
        &self,
        invocation_id: &HarnessInvocationId,
    ) -> Result<Option<HarnessInvocationView>, StoreError> {
        self.recover_one_harness_invocation(invocation_id).await
    }

    async fn recover_one_harness_invocation(
        &self,
        invocation_id: &HarnessInvocationId,
    ) -> Result<Option<HarnessInvocationView>, StoreError> {
        let mut tx = self.pool().begin().await?;
        // This helper owns at most one command lock. Every recovery candidate
        // therefore follows the frozen command-first order without ever
        // acquiring a classification-ordered multi-command lock set.
        let (command_state, view) =
            lock_harness_invocation_with_command(&mut tx, invocation_id).await?;
        let command = sqlx::query(
            "SELECT completion_idempotency_key, result FROM agent_commands WHERE id = $1",
        )
        .bind(view.command_id.as_uuid())
        .fetch_one(&mut *tx)
        .await?;
        let recovered = if command_state == "cancelled" && !view.terminal {
            Some(settle_harness_cancelled(&mut tx, &view).await?)
        } else if command_state == "completed" && !view.terminal {
            let result = command.get::<Option<Value>, _>("result").ok_or_else(|| {
                StoreError::InvalidData("completed harness command result is absent".to_owned())
            })?;
            let (state, payload) = terminal_from_completed_command(result)?;
            Some(
                update_invocation_state(
                    &mut tx,
                    &view.invocation_id,
                    state,
                    view.attempt,
                    view.prompt_accepted,
                    true,
                    Some(&terminal_idempotency_key(&view.invocation_id)),
                    Some(&payload),
                )
                .await?,
            )
        } else if view.state == HarnessInvocationState::Accepted && command_state == "claimed" {
            Some(view)
        } else if view.terminal
            && command
                .get::<Option<String>, _>("completion_idempotency_key")
                .is_none()
        {
            Some(view)
        } else if command_state == "claimed"
            && (view.state == HarnessInvocationState::Prepared
                || (view.state == HarnessInvocationState::Started && !view.prompt_accepted))
        {
            Some(view)
        } else {
            None
        };
        tx.commit().await?;
        Ok(recovered)
    }
}

fn transport_name(transport: HarnessTransport) -> &'static str {
    match transport {
        HarnessTransport::Builtin => "builtin",
        HarnessTransport::AcpStdio => "acp_stdio",
    }
}

fn terminal_state_matches_command_winner(
    state: HarnessInvocationState,
    command_state: &str,
) -> bool {
    match state {
        HarnessInvocationState::Completed | HarnessInvocationState::OutcomeUnknown => {
            command_state == "completed"
        }
        HarnessInvocationState::Failed => matches!(command_state, "completed" | "failed"),
        HarnessInvocationState::Cancelled => {
            matches!(command_state, "claimed" | "completed" | "cancelled")
        }
        HarnessInvocationState::Prepared
        | HarnessInvocationState::Started
        | HarnessInvocationState::Accepted => false,
    }
}

fn terminal_from_completed_command(
    result: Value,
) -> Result<(HarnessInvocationState, Value), StoreError> {
    match serde_json::from_value::<RuntimeEvent>(result)
        .map_err(|error| StoreError::InvalidData(error.to_string()))?
    {
        RuntimeEvent::HarnessResult {
            outcome,
            fields,
            history,
            ..
        } => Ok((
            HarnessInvocationState::Completed,
            serde_json::json!({
                "type": "completed",
                "outcome": outcome,
                "fields": fields,
                "history": history,
            }),
        )),
        RuntimeEvent::CommandFailed { code, .. } if code == "harness_outcome_unknown" => Ok((
            HarnessInvocationState::OutcomeUnknown,
            canonical_outcome_unknown_terminal_payload(),
        )),
        RuntimeEvent::CommandFailed { code, .. } if code == "harness_cancelled" => Ok((
            HarnessInvocationState::Cancelled,
            serde_json::json!({"type": "cancelled"}),
        )),
        RuntimeEvent::CommandFailed { code, .. } => Ok((
            HarnessInvocationState::Failed,
            serde_json::json!({
                "type": "failed",
                "code": if matches!(
                    code.as_str(),
                    "harness_profile_unavailable"
                        | "harness_protocol_incompatible"
                        | "harness_capability_unsupported"
                        | "harness_permission_rejected"
                        | "harness_failed"
                ) {
                    code
                } else {
                    "harness_failed".to_owned()
                },
                "detail": "harness command completed before terminal journal settlement",
            }),
        )),
        _ => Err(StoreError::InvalidData(
            "completed harness command result is incompatible".to_owned(),
        )),
    }
}

fn transport_from_row(value: &str) -> Result<HarnessTransport, StoreError> {
    match value {
        "builtin" => Ok(HarnessTransport::Builtin),
        "acp_stdio" => Ok(HarnessTransport::AcpStdio),
        _ => Err(StoreError::InvalidData(
            "harness transport is malformed".to_owned(),
        )),
    }
}

fn registration_from_row(row: &PgRow) -> Result<HarnessPluginRegistration, StoreError> {
    Ok(HarnessPluginRegistration {
        profile_id: HarnessProfileId::new(row.get::<String, _>("profile_id"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        profile_digest: row.get("profile_digest"),
        source_repository: row.get("source_repository"),
        source_revision: row.get("source_revision"),
        transport: transport_from_row(row.get("transport"))?,
        enabled: row.get("enabled"),
    })
}

fn harness_invocation_from_row(row: &PgRow) -> Result<HarnessInvocationView, StoreError> {
    let state = HarnessInvocationState::parse(row.get("state"))?;
    let terminal: bool = row.get("terminal");
    let terminal_request_digest: Option<String> = row.get("terminal_request_digest");
    let terminal_payload: Option<Value> = row.get("terminal_payload");
    if terminal != state.is_terminal()
        || (terminal && (terminal_request_digest.is_none() || terminal_payload.is_none()))
        || (!terminal && (terminal_request_digest.is_some() || terminal_payload.is_some()))
    {
        return Err(StoreError::InvalidData(
            "harness invocation terminal state is malformed".to_owned(),
        ));
    }
    Ok(HarnessInvocationView {
        invocation_id: HarnessInvocationId::new(row.get("invocation_id")),
        command_id: AgentCommandId::new(row.get("command_id")),
        command_revision: u64::try_from(row.get::<i64, _>("command_revision")).map_err(|_| {
            StoreError::InvalidData("harness command revision is malformed".to_owned())
        })?,
        profile_id: HarnessProfileId::new(row.get::<String, _>("profile_id"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        profile_digest: row.get("profile_digest"),
        payload_digest: row.get("payload_digest"),
        state,
        attempt: u64::try_from(row.get::<i64, _>("attempt"))
            .map_err(|_| StoreError::InvalidData("harness attempt is malformed".to_owned()))?,
        prompt_accepted: row.get("prompt_accepted"),
        terminal,
        terminal_request_digest,
        terminal_payload,
    })
}

fn harness_event_record_from_row(row: &PgRow) -> Result<HarnessInvocationEventRecord, StoreError> {
    let record = HarnessInvocationEventRecord {
        invocation_id: HarnessInvocationId::new(row.get("invocation_id")),
        sequence: u64::try_from(row.get::<i64, _>("sequence")).map_err(|_| {
            StoreError::InvalidData("harness event sequence is malformed".to_owned())
        })?,
        event_digest: row.get("event_digest"),
        event: harness_event_from_value(row.get::<Value, _>("event"))?,
    };
    record.validate_digest()?;
    Ok(record)
}

fn validate_invocation_identity_replay(
    view: &HarnessInvocationView,
    request: &PrepareHarnessInvocation<'_>,
) -> Result<(), StoreError> {
    if view.invocation_id != *request.invocation_id {
        return Err(StoreError::IdempotencyConflict);
    }
    validate_command_revision_replay(view, request)
}

fn validate_command_revision_replay(
    view: &HarnessInvocationView,
    request: &PrepareHarnessInvocation<'_>,
) -> Result<(), StoreError> {
    if view.command_id != *request.command_id
        || view.command_revision != request.command_revision
        || view.profile_id != *request.profile_id
        || view.profile_digest != request.profile_digest
        || view.payload_digest != request.payload_digest
    {
        return Err(StoreError::IdempotencyConflict);
    }
    Ok(())
}

async fn lock_harness_invocation(
    tx: &mut Transaction<'_, Postgres>,
    invocation_id: &HarnessInvocationId,
) -> Result<HarnessInvocationView, StoreError> {
    let row = sqlx::query(
        "SELECT invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
                state, attempt, prompt_accepted, terminal, terminal_request_digest, terminal_payload \
         FROM harness_invocations WHERE invocation_id = $1 FOR UPDATE",
    )
    .bind(invocation_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::InvalidData("harness invocation is absent".to_owned()))?;
    harness_invocation_from_row(&row)
}

async fn lock_harness_invocation_with_command(
    tx: &mut Transaction<'_, Postgres>,
    invocation_id: &HarnessInvocationId,
) -> Result<(String, HarnessInvocationView), StoreError> {
    let command_id: uuid::Uuid =
        sqlx::query_scalar("SELECT command_id FROM harness_invocations WHERE invocation_id = $1")
            .bind(invocation_id.as_uuid())
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| StoreError::InvalidData("harness invocation is absent".to_owned()))?;
    let command_state: String =
        sqlx::query_scalar("SELECT state FROM agent_commands WHERE id = $1 FOR UPDATE")
            .bind(command_id)
            .fetch_one(&mut **tx)
            .await?;
    let invocation = lock_harness_invocation(tx, invocation_id).await?;
    Ok((command_state, invocation))
}

async fn settle_harness_cancelled(
    tx: &mut Transaction<'_, Postgres>,
    view: &HarnessInvocationView,
) -> Result<HarnessInvocationView, StoreError> {
    if view.terminal {
        return Ok(view.clone());
    }
    let payload = serde_json::json!({"type": "cancelled"});
    update_invocation_state(
        tx,
        &view.invocation_id,
        HarnessInvocationState::Cancelled,
        view.attempt,
        view.prompt_accepted,
        true,
        Some(&terminal_idempotency_key(&view.invocation_id)),
        Some(&payload),
    )
    .await
}

pub(super) async fn cancel_harness_invocations_for_task(
    tx: &mut Transaction<'_, Postgres>,
    task_id: &UserTaskId,
) -> Result<(), StoreError> {
    let rows = sqlx::query(
        "SELECT invocations.invocation_id, invocations.command_id, invocations.command_revision, \
                invocations.profile_id, invocations.profile_digest, invocations.payload_digest, \
                invocations.state, invocations.attempt, invocations.prompt_accepted, \
                invocations.terminal, invocations.terminal_request_digest, invocations.terminal_payload \
         FROM harness_invocations invocations \
         JOIN agent_commands commands ON commands.id = invocations.command_id \
         WHERE commands.task_id = $1 AND invocations.terminal = FALSE \
         ORDER BY commands.id, invocations.invocation_id FOR UPDATE OF invocations",
    )
    .bind(task_id.as_uuid())
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let view = harness_invocation_from_row(&row)?;
        settle_harness_cancelled(tx, &view).await?;
    }
    Ok(())
}

async fn update_invocation_state(
    tx: &mut Transaction<'_, Postgres>,
    invocation_id: &HarnessInvocationId,
    state: HarnessInvocationState,
    attempt: u64,
    prompt_accepted: bool,
    terminal: bool,
    terminal_request_digest: Option<&str>,
    terminal_payload: Option<&Value>,
) -> Result<HarnessInvocationView, StoreError> {
    let row = sqlx::query(
        "UPDATE harness_invocations SET state = $1, attempt = $2, prompt_accepted = $3, terminal = $4, \
                terminal_request_digest = $5, terminal_payload = $6, updated_at = CURRENT_TIMESTAMP \
         WHERE invocation_id = $7 \
         RETURNING invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
                   state, attempt, prompt_accepted, terminal, terminal_request_digest, terminal_payload",
    )
    .bind(state.as_str())
    .bind(harness_i64(attempt, "harness attempt is malformed")?)
    .bind(prompt_accepted)
    .bind(terminal)
    .bind(terminal_request_digest)
    .bind(terminal_payload)
    .bind(invocation_id.as_uuid())
    .fetch_one(&mut **tx)
    .await?;
    harness_invocation_from_row(&row)
}

fn harness_i64(value: u64, message: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidData(message.to_owned()))
}

fn is_harness_invocation_identity_conflict(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(|database_error| {
        database_error.code().as_deref() == Some("23505")
            && database_error.constraint() == Some("harness_invocations_pkey")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        terminal_from_completed_command, terminal_state_matches_command_winner,
        validate_command_revision_replay, validate_invocation_identity_replay,
    };
    use crate::harness::{HarnessInvocationId, HarnessProfileId};
    use crate::store::{HarnessInvocationState, HarnessInvocationView, PrepareHarnessInvocation};
    use crate::{AgentCommandId, EvidenceId, RuntimeCommandKey, RuntimeEvent, StoreError};

    fn stored_invocation() -> HarnessInvocationView {
        HarnessInvocationView {
            invocation_id: HarnessInvocationId::new(uuid::Uuid::now_v7()),
            command_id: AgentCommandId::new(uuid::Uuid::now_v7()),
            command_revision: 7,
            profile_id: HarnessProfileId::new("builtin-v1").unwrap(),
            profile_digest: "profile-digest".to_owned(),
            payload_digest: "payload-digest".to_owned(),
            state: HarnessInvocationState::Completed,
            attempt: 0,
            prompt_accepted: true,
            terminal: true,
            terminal_request_digest: Some("terminal-key".to_owned()),
            terminal_payload: Some(serde_json::json!({"type":"completed"})),
        }
    }

    #[test]
    fn command_revision_replay_discards_a_fresh_candidate_but_uuid_reuse_checks_command_identity() {
        let stored = stored_invocation();
        let fresh_candidate = HarnessInvocationId::new(uuid::Uuid::now_v7());
        let replay = PrepareHarnessInvocation {
            invocation_id: &fresh_candidate,
            command_id: &stored.command_id,
            command_revision: stored.command_revision,
            profile_id: &stored.profile_id,
            profile_digest: &stored.profile_digest,
            payload_digest: &stored.payload_digest,
        };
        validate_command_revision_replay(&stored, &replay)
            .expect("command-bound replay ignores its disposable candidate UUID");

        let different_command = AgentCommandId::new(uuid::Uuid::now_v7());
        let reused_uuid = PrepareHarnessInvocation {
            invocation_id: &stored.invocation_id,
            command_id: &different_command,
            command_revision: stored.command_revision,
            profile_id: &stored.profile_id,
            profile_digest: &stored.profile_digest,
            payload_digest: &stored.payload_digest,
        };
        assert!(matches!(
            validate_invocation_identity_replay(&stored, &reused_uuid),
            Err(StoreError::IdempotencyConflict)
        ));
    }

    #[test]
    fn terminal_journal_requires_the_command_cas_winner() {
        assert!(!terminal_state_matches_command_winner(
            HarnessInvocationState::Completed,
            "claimed"
        ));
        assert!(terminal_state_matches_command_winner(
            HarnessInvocationState::Completed,
            "completed"
        ));
        assert!(terminal_state_matches_command_winner(
            HarnessInvocationState::Cancelled,
            "cancelled"
        ));
        assert!(terminal_state_matches_command_winner(
            HarnessInvocationState::Failed,
            "completed"
        ));
        assert!(terminal_state_matches_command_winner(
            HarnessInvocationState::OutcomeUnknown,
            "completed"
        ));
    }

    #[test]
    fn completed_command_result_reconstructs_the_missing_harness_terminal() {
        let event = RuntimeEvent::HarnessResult {
            event_id: "harness-result:test".to_owned(),
            command_key: RuntimeCommandKey::new("harness:test").unwrap(),
            outcome: "ok".to_owned(),
            fields: serde_json::json!({"answer":"stable"}),
            evidence_id: EvidenceId::new(uuid::Uuid::now_v7()),
            history: vec![],
        };
        let (state, payload) =
            terminal_from_completed_command(serde_json::to_value(event).unwrap()).unwrap();
        assert_eq!(state, HarnessInvocationState::Completed);
        assert_eq!(
            payload,
            serde_json::json!({
                "type":"completed",
                "outcome":"ok",
                "fields":{"answer":"stable"},
                "history":[],
            })
        );
    }
}
