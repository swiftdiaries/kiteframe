use serde_json::Value;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};

use crate::AgentCommandId;
use crate::harness::{
    HarnessEventEnvelope, HarnessInvocationId, HarnessProfileId, HarnessTransport,
};

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

    pub async fn prepare_harness_invocation(
        &self,
        request: PrepareHarnessInvocation<'_>,
    ) -> Result<HarnessInvocationView, StoreError> {
        let command_revision = harness_i64(
            request.command_revision,
            "harness command revision is malformed",
        )?;
        let mut tx = self.pool().begin().await?;
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
                    state, attempt, prompt_accepted, terminal, terminal_request_digest \
             FROM harness_invocations WHERE invocation_id = $1 FOR UPDATE",
        )
        .bind(request.invocation_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        {
            let view = harness_invocation_from_row(&row)?;
            validate_prepare_replay(&view, &request)?;
            tx.commit().await?;
            return Ok(view);
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
                    state, attempt, prompt_accepted, terminal, terminal_request_digest \
             FROM harness_invocations WHERE command_id = $1 AND command_revision = $2 FOR UPDATE",
        )
        .bind(request.command_id.as_uuid())
        .bind(command_revision)
        .fetch_one(&mut *tx)
        .await?;
        let view = harness_invocation_from_row(&row)?;
        validate_prepare_replay(&view, &request)?;
        tx.commit().await?;
        Ok(view)
    }

    pub async fn mark_harness_invocation_started(
        &self,
        invocation_id: &HarnessInvocationId,
        expected_attempt: u64,
    ) -> Result<HarnessInvocationView, StoreError> {
        let mut tx = self.pool().begin().await?;
        let view = lock_harness_invocation(&mut tx, invocation_id).await?;
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
    ) -> Result<HarnessInvocationView, StoreError> {
        if request_digest.is_empty() {
            return Err(StoreError::InvalidData(
                "harness retryable failure request is malformed".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let view = lock_harness_invocation(&mut tx, invocation_id).await?;
        if view.state == HarnessInvocationState::Failed && view.attempt == expected_attempt {
            if view.terminal_request_digest.as_deref() == Some(request_digest) {
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
        let view = lock_harness_invocation(&mut tx, invocation_id).await?;
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
            "SELECT event_digest FROM harness_invocation_events \
             WHERE invocation_id = $1 AND sequence = $2 FOR UPDATE",
        )
        .bind(event.invocation_id.as_uuid())
        .bind(sequence)
        .fetch_optional(&mut *tx)
        .await?
        {
            if existing.get::<String, _>("event_digest") == event.event_digest {
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
        let stored: String = sqlx::query_scalar(
            "SELECT event_digest FROM harness_invocation_events \
             WHERE invocation_id = $1 AND sequence = $2 FOR UPDATE",
        )
        .bind(event.invocation_id.as_uuid())
        .bind(sequence)
        .fetch_one(&mut *tx)
        .await?;
        if stored != event.event_digest {
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
    ) -> Result<HarnessInvocationView, StoreError> {
        if !state.is_terminal() || request_digest.is_empty() {
            return Err(StoreError::InvalidData(
                "harness terminal settlement is malformed".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let view = lock_harness_invocation(&mut tx, invocation_id).await?;
        if view.terminal {
            if view.state == state
                && view.terminal_request_digest.as_deref() == Some(request_digest)
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
        )
        .await?;
        tx.commit().await?;
        Ok(view)
    }

    pub async fn recover_harness_invocations(
        &self,
        limit: i64,
    ) -> Result<Vec<HarnessInvocationView>, StoreError> {
        let mut tx = self.pool().begin().await?;
        let accepted_rows = sqlx::query(
            "SELECT invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
                    state, attempt, prompt_accepted, terminal, terminal_request_digest \
             FROM harness_invocations \
             WHERE state = 'accepted' ORDER BY created_at, invocation_id FOR UPDATE",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut recovered = Vec::with_capacity(accepted_rows.len() + limit.clamp(1, 256) as usize);
        for row in accepted_rows {
            let view = harness_invocation_from_row(&row)?;
            recovered.push(
                update_invocation_state(
                    &mut tx,
                    &view.invocation_id,
                    HarnessInvocationState::OutcomeUnknown,
                    view.attempt,
                    true,
                    true,
                    Some("harness-recovery-outcome-unknown"),
                )
                .await?,
            );
        }
        let retryable_rows = sqlx::query(
            "SELECT invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
                    state, attempt, prompt_accepted, terminal, terminal_request_digest \
             FROM harness_invocations \
             WHERE state = 'prepared' OR (state = 'started' AND prompt_accepted = FALSE) \
             ORDER BY created_at, invocation_id LIMIT $1 FOR UPDATE",
        )
        .bind(limit.clamp(1, 256))
        .fetch_all(&mut *tx)
        .await?;
        for row in retryable_rows {
            recovered.push(harness_invocation_from_row(&row)?);
        }
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
    if terminal != state.is_terminal()
        || (terminal && terminal_request_digest.is_none())
        || (!terminal && terminal_request_digest.is_some())
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

fn validate_prepare_replay(
    view: &HarnessInvocationView,
    request: &PrepareHarnessInvocation<'_>,
) -> Result<(), StoreError> {
    if view.invocation_id != *request.invocation_id
        || view.command_id != *request.command_id
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
                state, attempt, prompt_accepted, terminal, terminal_request_digest \
         FROM harness_invocations WHERE invocation_id = $1 FOR UPDATE",
    )
    .bind(invocation_id.as_uuid())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StoreError::InvalidData("harness invocation is absent".to_owned()))?;
    harness_invocation_from_row(&row)
}

async fn update_invocation_state(
    tx: &mut Transaction<'_, Postgres>,
    invocation_id: &HarnessInvocationId,
    state: HarnessInvocationState,
    attempt: u64,
    prompt_accepted: bool,
    terminal: bool,
    terminal_request_digest: Option<&str>,
) -> Result<HarnessInvocationView, StoreError> {
    let row = sqlx::query(
        "UPDATE harness_invocations SET state = $1, attempt = $2, prompt_accepted = $3, terminal = $4, \
                terminal_request_digest = $5, updated_at = CURRENT_TIMESTAMP \
         WHERE invocation_id = $6 \
         RETURNING invocation_id, command_id, command_revision, profile_id, profile_digest, payload_digest, \
                   state, attempt, prompt_accepted, terminal, terminal_request_digest",
    )
    .bind(state.as_str())
    .bind(harness_i64(attempt, "harness attempt is malformed")?)
    .bind(prompt_accepted)
    .bind(terminal)
    .bind(terminal_request_digest)
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
