use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

use crate::dsl::CompiledWorkflow;
use crate::{
    CatalogEpochId, PolicyRevision, PrincipalId, SessionId, ToolCatalogSnapshot, canonical_sha256,
};

use super::{
    CreateSession, PolicyUpdate, PublishedDefinition, RunState, RunView, SessionView, StartRun,
    StoreError,
};

pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn create_session(&self, request: CreateSession) -> Result<SessionView, StoreError> {
        let id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO sessions (id, principal, working_directory, retention_until) VALUES ($1, $2, $3, $4)")
            .bind(id).bind(request.principal.as_str()).bind(request.working_directory).bind(request.retention_until)
            .execute(&self.pool).await?;
        Ok(SessionView {
            id: SessionId::new(id),
            principal: request.principal,
            policy_revision: PolicyRevision::new(0),
            current_catalog_epoch: None,
            mcp_attached: false,
        })
    }

    pub async fn resume_session(
        &self,
        id: SessionId,
        principal: &PrincipalId,
    ) -> Result<SessionView, StoreError> {
        let row = sqlx::query("SELECT principal, state, current_policy_revision, current_catalog_epoch FROM sessions WHERE id = $1")
            .bind(*id.as_uuid()).fetch_optional(&self.pool).await?.ok_or(StoreError::SessionNotFound)?;
        if row.get::<String, _>("principal") != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if row.get::<String, _>("state") != "active" {
            return Err(StoreError::SessionClosed);
        }
        let epoch = row
            .get::<Option<uuid::Uuid>, _>("current_catalog_epoch")
            .map(CatalogEpochId::new);
        Ok(SessionView {
            id,
            principal: principal.clone(),
            policy_revision: PolicyRevision::new(
                row.get::<i64, _>("current_policy_revision") as u64
            ),
            mcp_attached: epoch.is_some(),
            current_catalog_epoch: epoch,
        })
    }

    pub async fn append_catalog_epoch(
        &self,
        epoch: &ToolCatalogSnapshot,
    ) -> Result<(), StoreError> {
        let snapshot = catalog_json(epoch);
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query("SELECT 1 FROM sessions WHERE id = $1 FOR UPDATE")
            .bind(*epoch.session_id().as_uuid())
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Err(StoreError::SessionNotFound);
        }
        if let Some(row) = sqlx::query(
            "SELECT session_id, canonical_digest FROM mcp_catalog_epochs WHERE epoch_id = $1",
        )
        .bind(*epoch.epoch_id().as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        {
            if row.get::<uuid::Uuid, _>("session_id") != *epoch.session_id().as_uuid()
                || row.get::<String, _>("canonical_digest") != epoch.digest()
            {
                return Err(StoreError::CatalogScopeViolation);
            }
        } else {
            sqlx::query("INSERT INTO mcp_catalog_epochs (epoch_id, session_id, server_alias, refresh_identity, canonical_digest, connection_binding, descriptor_snapshot) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(*epoch.epoch_id().as_uuid()).bind(*epoch.session_id().as_uuid()).bind("session_catalog").bind(epoch.epoch_id().as_uuid().to_string()).bind(epoch.digest()).bind(json!({"kind":"session_catalog"})).bind(snapshot)
            .execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE sessions SET current_catalog_epoch = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2").bind(*epoch.epoch_id().as_uuid()).bind(*epoch.session_id().as_uuid()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn append_policy_rules(
        &self,
        update: PolicyUpdate,
    ) -> Result<PolicyRevision, StoreError> {
        let digest = canonical_sha256(&update.typed_source)
            .map_err(|e| StoreError::InvalidData(e.to_string()))?;
        let mut tx = self.pool.begin().await?;
        let row =
            sqlx::query("SELECT current_policy_revision FROM sessions WHERE id = $1 FOR UPDATE")
                .bind(*update.session.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::SessionNotFound)?;
        if let Some(existing) = sqlx::query("SELECT source_digest, revision FROM session_policy_rules WHERE session_id = $1 AND idempotency_key = $2").bind(*update.session.as_uuid()).bind(&update.idempotency_key).fetch_optional(&mut *tx).await? {
            if existing.get::<String, _>("source_digest") == digest { return Ok(PolicyRevision::new(existing.get::<i64, _>("revision") as u64)); }
            return Err(StoreError::IdempotencyConflict);
        }
        let current = row.get::<i64, _>("current_policy_revision") as u64;
        if current != update.expected_revision.get() {
            return Err(StoreError::PolicyRevisionConflict);
        }
        let revision = current + 1;
        sqlx::query("INSERT INTO session_policy_rules (id, session_id, revision, idempotency_key, typed_source, compiled_cedar, source_digest) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(uuid::Uuid::now_v7()).bind(*update.session.as_uuid()).bind(revision as i64).bind(update.idempotency_key).bind(update.typed_source).bind(update.compiled_cedar).bind(digest).execute(&mut *tx).await?;
        sqlx::query("UPDATE sessions SET current_policy_revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2").bind(revision as i64).bind(*update.session.as_uuid()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(PolicyRevision::new(revision))
    }

    pub async fn append_session_policy(
        &self,
        session: SessionId,
        expected_revision: PolicyRevision,
        idempotency_key: String,
        policy: &crate::SessionPolicy,
        catalog: &ToolCatalogSnapshot,
    ) -> Result<PolicyRevision, StoreError> {
        if catalog.session_id() != &session {
            return Err(StoreError::CatalogScopeViolation);
        }
        let compiled = crate::policy::compile(policy, catalog)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let typed_source = serde_json::to_value(policy)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let compiled_cedar = serde_json::to_string(&compiled)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        self.append_policy_rules(PolicyUpdate {
            session,
            expected_revision,
            idempotency_key,
            typed_source,
            compiled_cedar,
        })
        .await
    }

    pub async fn start_run(&self, request: StartRun) -> Result<RunView, StoreError> {
        let input_digest = canonical_sha256(&request.input)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let bindings = request
            .input
            .as_object()
            .ok_or_else(|| StoreError::InvalidData("workflow input must be a JSON object".to_owned()))?;
        let mut tx = self.pool.begin().await?;
        let session = sqlx::query(
            "SELECT state, current_catalog_epoch FROM sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(*request.session_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::SessionNotFound)?;
        if session.get::<String, _>("state") != "active" {
            return Err(StoreError::SessionClosed);
        }
        let catalog_epoch = session
            .get::<Option<uuid::Uuid>, _>("current_catalog_epoch")
            .ok_or(StoreError::CatalogScopeViolation)?;
        let definition = if let Some(version) = request.version {
            sqlx::query("SELECT id, name, version, definition_hash, tool_bindings, compiled_definition FROM workflow_definitions WHERE name = $1 AND version = $2")
                .bind(&request.workflow_name)
                .bind(version as i32)
                .fetch_optional(&mut *tx)
                .await?
        } else {
            sqlx::query("SELECT id, name, version, definition_hash, tool_bindings, compiled_definition FROM workflow_definitions WHERE name = $1 ORDER BY version DESC LIMIT 1")
                .bind(&request.workflow_name)
                .fetch_optional(&mut *tx)
                .await?
        }
        .ok_or(StoreError::DefinitionNotFound)?;
        let definition_id = definition.get::<uuid::Uuid, _>("id");
        if let Some(existing) = sqlx::query(
            "SELECT id, definition_id, input_digest, state, revision FROM runs WHERE session_id = $1 AND start_idempotency_key = $2",
        )
        .bind(*request.session_id.as_uuid())
        .bind(request.idempotency_key.as_str())
        .fetch_optional(&mut *tx)
        .await?
        {
            if existing.get::<uuid::Uuid, _>("definition_id") != definition_id
                || existing.get::<String, _>("input_digest") != input_digest
            {
                return Err(StoreError::IdempotencyConflict);
            }
            let root = sqlx::query(
                "SELECT id FROM frames WHERE run_id = $1 AND parent_frame_id IS NULL",
            )
            .bind(existing.get::<uuid::Uuid, _>("id"))
            .fetch_one(&mut *tx)
            .await?;
            return Ok(RunView {
                id: crate::RunId::new(existing.get("id")),
                session_id: request.session_id,
                definition: definition_from_row(&definition),
                root_frame_id: crate::FrameId::new(root.get("id")),
                state: run_state(&existing.get::<String, _>("state"))?,
                revision: existing.get::<i64, _>("revision") as u64,
            });
        }
        let catalog = sqlx::query(
            "SELECT descriptor_snapshot FROM mcp_catalog_epochs WHERE epoch_id = $1 AND session_id = $2",
        )
        .bind(catalog_epoch)
        .bind(*request.session_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::CatalogScopeViolation)?
        .get::<Value, _>("descriptor_snapshot");
        let catalog_tools = catalog
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| StoreError::InvalidData("catalog snapshot tools are malformed".to_owned()))?;
        let definition_bindings = definition.get::<Value, _>("tool_bindings");
        let definition_bindings = definition_bindings
            .as_array()
            .ok_or_else(|| StoreError::InvalidData("workflow tool bindings are malformed".to_owned()))?;
        let run_id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO runs (id, session_id, definition_id, start_idempotency_key, input, input_digest, state) VALUES ($1, $2, $3, $4, $5, $6, 'runnable')")
            .bind(run_id).bind(*request.session_id.as_uuid()).bind(definition_id).bind(request.idempotency_key.as_str()).bind(&request.input).bind(&input_digest).execute(&mut *tx).await?;
        for binding in definition_bindings {
            let qualified_name = binding
                .get("qualified_name")
                .and_then(Value::as_str)
                .ok_or_else(|| StoreError::InvalidData("workflow binding name is malformed".to_owned()))?;
            let descriptor_digest = binding
                .get("descriptor_digest")
                .and_then(Value::as_str)
                .ok_or_else(|| StoreError::InvalidData("workflow binding digest is malformed".to_owned()))?;
            let binding_epoch = binding
                .get("catalog_epoch_id")
                .and_then(Value::as_str)
                .ok_or_else(|| StoreError::InvalidData("workflow binding epoch is malformed".to_owned()))?;
            if binding_epoch != catalog_epoch.to_string() {
                return Err(StoreError::CatalogScopeViolation);
            }
            let tool = catalog_tools.iter().find_map(|entry| {
                let descriptor = entry.get("descriptor")?;
                (entry.get("qualified_name")?.as_str() == Some(qualified_name)
                    && entry.get("descriptor_digest")?.as_str() == Some(descriptor_digest))
                    .then_some(descriptor)
            }).ok_or(StoreError::CatalogScopeViolation)?;
            sqlx::query("INSERT INTO run_tool_bindings (run_id, qualified_name, server_alias, catalog_epoch_id, descriptor_digest, capabilities, recovery_profile) VALUES ($1, $2, $3, $4, $5, $6, $7)")
                .bind(run_id).bind(qualified_name).bind(tool.get("server_alias").and_then(Value::as_str).ok_or_else(|| StoreError::InvalidData("catalog tool server alias is malformed".to_owned()))?).bind(catalog_epoch).bind(descriptor_digest).bind(tool.get("capabilities").cloned().ok_or_else(|| StoreError::InvalidData("catalog tool capabilities are malformed".to_owned()))?).bind(tool.get("recovery").and_then(Value::as_str).ok_or_else(|| StoreError::InvalidData("catalog tool recovery profile is malformed".to_owned()))?).execute(&mut *tx).await?;
        }
        let frame_id = uuid::Uuid::now_v7();
        let compiled_definition = definition.get::<Value, _>("compiled_definition");
        let task_name = compiled_definition
            .get("entry")
            .and_then(|entry| entry.get("task"))
            .and_then(Value::as_str)
            .ok_or_else(|| StoreError::InvalidData("compiled workflow entry is malformed".to_owned()))?;
        sqlx::query("INSERT INTO frames (id, run_id, task_name, bindings, state) VALUES ($1, $2, $3, $4, 'runnable')")
            .bind(frame_id).bind(run_id).bind(task_name).bind(Value::Object(bindings.clone())).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO events (run_id, sequence, kind, metadata) VALUES ($1, 1, 'run.started', $2)")
            .bind(run_id).bind(json!({"definition_id": definition_id, "catalog_epoch_id": catalog_epoch})).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(RunView {
            id: crate::RunId::new(run_id),
            session_id: request.session_id,
            definition: definition_from_row(&definition),
            root_frame_id: crate::FrameId::new(frame_id),
            state: RunState::Runnable,
            revision: 0,
        })
    }

    pub async fn begin_model_attempt(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        input: Value,
    ) -> Result<super::ModelAttempt, StoreError> {
        let mut tx = self.pool.begin().await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision {
            return Err(StoreError::RevisionConflict);
        }
        if frame.get::<String, _>("state") != "runnable" {
            return Err(StoreError::RevisionConflict);
        }
        let run_id = frame.get::<uuid::Uuid, _>("run_id");
        sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE")
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
        let attempt = sqlx::query("SELECT COALESCE(MAX(attempt), 0) + 1 AS attempt FROM model_items WHERE frame_id = $1")
            .bind(*frame_id.as_uuid())
            .fetch_one(&mut *tx)
            .await?
            .get::<i32, _>("attempt") as u32;
        sqlx::query("INSERT INTO model_items (frame_id, attempt, position, direction, item) VALUES ($1, $2, 0, 'input', $3)")
            .bind(*frame_id.as_uuid()).bind(attempt as i32).bind(input).execute(&mut *tx).await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'running', revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'running', updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND state = 'runnable'")
            .bind(run_id).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "model.intent", json!({"frame_id": frame_id.as_uuid(), "attempt": attempt})).await?;
        tx.commit().await?;
        Ok(super::ModelAttempt { frame_id, attempt, revision })
    }

    pub async fn commit_model_attempt(
        &self,
        frame_id: crate::FrameId,
        attempt: u32,
        expected_revision: u64,
        items: Vec<Value>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
        let run_id = frame.get::<uuid::Uuid, _>("run_id");
        sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE")
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
        let intent = sqlx::query("SELECT 1 FROM model_items WHERE frame_id = $1 AND attempt = $2 AND position = 0 AND direction = 'input'")
            .bind(*frame_id.as_uuid()).bind(attempt as i32).fetch_optional(&mut *tx).await?;
        if intent.is_none() {
            return Err(StoreError::InvalidData("model result has no matching intent".to_owned()));
        }
        for (position, item) in items.into_iter().enumerate() {
            sqlx::query("INSERT INTO model_items (frame_id, attempt, position, direction, item) VALUES ($1, $2, $3, 'output', $4)")
                .bind(*frame_id.as_uuid()).bind(attempt as i32).bind(position as i32).bind(item).execute(&mut *tx).await?;
        }
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "model.committed", json!({"frame_id": frame_id.as_uuid(), "attempt": attempt})).await?;
        tx.commit().await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn begin_tool_call(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        provider_call_id: String,
        qualified_name: String,
        descriptor_digest: String,
        arguments: Value,
        idempotency_key: String,
    ) -> Result<super::ToolCallIntent, StoreError> {
        let mut tx = self.pool.begin().await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::FrameNotFound)?;
        let run_id = frame.get::<uuid::Uuid, _>("run_id");
        sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE")
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
        if let Some(existing) = sqlx::query("SELECT id, provider_call_id, qualified_name, descriptor_digest, arguments FROM tool_calls WHERE frame_id = $1 AND idempotency_key = $2")
            .bind(*frame_id.as_uuid()).bind(&idempotency_key).fetch_optional(&mut *tx).await?
        {
            let existing_arguments = existing.get::<Value, _>("arguments");
            if existing.get::<String, _>("provider_call_id") != provider_call_id
                || existing.get::<String, _>("qualified_name") != qualified_name
                || existing.get::<String, _>("descriptor_digest") != descriptor_digest
                || canonical_sha256(&existing_arguments).map_err(|error| StoreError::InvalidData(error.to_string()))?
                    != canonical_sha256(&arguments).map_err(|error| StoreError::InvalidData(error.to_string()))?
            {
                return Err(StoreError::IdempotencyConflict);
            }
            return Ok(super::ToolCallIntent {
                id: existing.get("id"),
                frame_id,
                provider_call_id,
                revision: frame.get::<i64, _>("revision") as u64,
            });
        }
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
        let binding = sqlx::query("SELECT 1 FROM run_tool_bindings WHERE run_id = $1 AND qualified_name = $2 AND descriptor_digest = $3")
            .bind(run_id).bind(&qualified_name).bind(&descriptor_digest).fetch_optional(&mut *tx).await?;
        if binding.is_none() {
            return Err(StoreError::CatalogScopeViolation);
        }
        let id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO tool_calls (id, frame_id, provider_call_id, qualified_name, descriptor_digest, idempotency_key, arguments, state) VALUES ($1, $2, $3, $4, $5, $6, $7, 'intent')")
            .bind(id).bind(*frame_id.as_uuid()).bind(&provider_call_id).bind(&qualified_name).bind(&descriptor_digest).bind(idempotency_key).bind(arguments).execute(&mut *tx).await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "tool.intent", json!({"frame_id": frame_id.as_uuid(), "tool_call_id": id, "qualified_name": qualified_name, "descriptor_digest": descriptor_digest})).await?;
        tx.commit().await?;
        Ok(super::ToolCallIntent { id, frame_id, provider_call_id, revision })
    }

    pub async fn finish_tool_call(
        &self,
        frame_id: crate::FrameId,
        tool_call_id: uuid::Uuid,
        expected_revision: u64,
        outcome: Result<Value, Value>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
        let run_id = frame.get::<uuid::Uuid, _>("run_id");
        sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE").bind(run_id).fetch_one(&mut *tx).await?;
        let call = sqlx::query("SELECT state FROM tool_calls WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(tool_call_id).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("tool call was not found for frame".to_owned()))?;
        if call.get::<String, _>("state") != "intent" {
            return Err(StoreError::RevisionConflict);
        }
        match outcome {
            Ok(result) => {
                sqlx::query("UPDATE tool_calls SET state = 'completed', result = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(result).bind(tool_call_id).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "tool.completed", json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id})).await?;
            }
            Err(error) => {
                sqlx::query("UPDATE tool_calls SET state = 'failed', error = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(error).bind(tool_call_id).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "tool.failed", json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id})).await?;
            }
        }
        sqlx::query("UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind((expected_revision + 1) as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create_permission_request(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        request: super::NewPermissionRequest,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let frame = sqlx::query("SELECT f.run_id, f.revision, f.state, r.session_id FROM frames f JOIN runs r ON r.id = f.run_id WHERE f.id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
        let run_id = frame.get::<uuid::Uuid, _>("run_id");
        sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE").bind(run_id).fetch_one(&mut *tx).await?;
        let policy = sqlx::query("SELECT current_policy_revision FROM sessions WHERE id = $1")
            .bind(frame.get::<uuid::Uuid, _>("session_id")).fetch_one(&mut *tx).await?;
        if policy.get::<i64, _>("current_policy_revision") as u64 != request.policy_revision.get() {
            return Err(StoreError::RevisionConflict);
        }
        let call = sqlx::query("SELECT arguments, state FROM tool_calls WHERE frame_id = $1 AND provider_call_id = $2 AND qualified_name = $3 AND descriptor_digest = $4 FOR UPDATE")
            .bind(*frame_id.as_uuid()).bind(&request.provider_call_id).bind(&request.qualified_tool).bind(&request.descriptor_digest).fetch_optional(&mut *tx).await?
            .ok_or(StoreError::CatalogScopeViolation)?;
        if call.get::<String, _>("state") != "intent"
            || canonical_sha256(&call.get::<Value, _>("arguments")).map_err(|error| StoreError::InvalidData(error.to_string()))? != request.input_digest
        {
            return Err(StoreError::CatalogScopeViolation);
        }
        sqlx::query("INSERT INTO permission_requests (id, run_id, frame_id, provider_call_id, qualified_tool, descriptor_digest, input_digest, policy_revision, state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending')")
            .bind(*request.id.as_uuid()).bind(run_id).bind(*frame_id.as_uuid()).bind(&request.provider_call_id).bind(&request.qualified_tool).bind(&request.descriptor_digest).bind(&request.input_digest).bind(request.policy_revision.get() as i64).execute(&mut *tx).await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'waiting_for_permission', revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'waiting_for_permission', updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(run_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE tool_calls SET state = 'waiting_for_permission', updated_at = CURRENT_TIMESTAMP WHERE frame_id = $1 AND provider_call_id = $2")
            .bind(*frame_id.as_uuid()).bind(&request.provider_call_id).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "permission.requested", json!({"frame_id": frame_id.as_uuid(), "request_id": request.id.as_uuid(), "provider_call_id": request.provider_call_id, "qualified_tool": request.qualified_tool, "descriptor_digest": request.descriptor_digest, "policy_revision": request.policy_revision.get()})).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn resolve_permission(
        &self,
        frame_id: crate::FrameId,
        request_id: crate::PermissionRequestId,
        expected_revision: u64,
        resolution: Value,
        idempotency_key: String,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        let run_id = frame.get::<uuid::Uuid, _>("run_id");
        sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE").bind(run_id).fetch_one(&mut *tx).await?;
        let request = sqlx::query("SELECT state, resolution_idempotency_key FROM permission_requests WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(*request_id.as_uuid()).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("permission request was not found for frame".to_owned()))?;
        if request.get::<String, _>("state") == "resolved" {
            if request.get::<Option<String>, _>("resolution_idempotency_key") == Some(idempotency_key) {
                return Ok(());
            }
            return Err(StoreError::IdempotencyConflict);
        }
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "waiting_for_permission"
            || request.get::<String, _>("state") != "pending"
        {
            return Err(StoreError::RevisionConflict);
        }
        sqlx::query("UPDATE permission_requests SET state = 'resolved', resolution = $1, resolution_idempotency_key = $2, resolved_at = CURRENT_TIMESTAMP WHERE id = $3")
            .bind(resolution).bind(idempotency_key).bind(*request_id.as_uuid()).execute(&mut *tx).await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'running', revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'running', updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(run_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE tool_calls SET state = 'intent', updated_at = CURRENT_TIMESTAMP WHERE frame_id = $1 AND provider_call_id = (SELECT provider_call_id FROM permission_requests WHERE id = $2)")
            .bind(*frame_id.as_uuid()).bind(*request_id.as_uuid()).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "permission.resolved", json!({"frame_id": frame_id.as_uuid(), "request_id": request_id.as_uuid()})).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn publish(
        &self,
        session: SessionId,
        workflow: &CompiledWorkflow,
        source: &str,
    ) -> Result<PublishedDefinition, StoreError> {
        self.verify_bindings(session, workflow).await?;
        let compiled =
            serde_json::to_value(workflow).map_err(|e| StoreError::InvalidData(e.to_string()))?;
        let bindings = bindings_json(workflow);
        let mut tx = self.pool.begin().await?;
        if let Some(row) = sqlx::query("SELECT id, name, version, definition_hash FROM workflow_definitions WHERE definition_hash = $1").bind(&workflow.definition_hash).fetch_optional(&mut *tx).await? {
            return Ok(definition_from_row(&row));
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&workflow.name)
            .execute(&mut *tx)
            .await?;
        let version: i32 = sqlx::query(
            "SELECT COALESCE(MAX(version), 0) + 1 AS version FROM workflow_definitions WHERE name = $1",
        )
        .bind(&workflow.name)
        .fetch_one(&mut *tx)
        .await?
        .get("version");
        let id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO workflow_definitions (id, name, version, definition_hash, source, compiled_definition, tool_bindings) VALUES ($1, $2, $3, $4, $5, $6, $7)").bind(id).bind(&workflow.name).bind(version).bind(&workflow.definition_hash).bind(source).bind(compiled).bind(bindings).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(PublishedDefinition {
            id,
            name: workflow.name.clone(),
            version: version as u32,
            definition_hash: workflow.definition_hash.clone(),
        })
    }

    async fn verify_bindings(
        &self,
        session: SessionId,
        workflow: &CompiledWorkflow,
    ) -> Result<(), StoreError> {
        let row = sqlx::query("SELECT current_catalog_epoch FROM sessions WHERE id = $1")
            .bind(*session.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        let current = row
            .get::<Option<uuid::Uuid>, _>("current_catalog_epoch")
            .ok_or(StoreError::CatalogScopeViolation)?;
        let snapshot = sqlx::query("SELECT descriptor_snapshot FROM mcp_catalog_epochs WHERE epoch_id = $1 AND session_id = $2")
            .bind(current)
            .bind(*session.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::CatalogScopeViolation)?
            .get::<Value, _>("descriptor_snapshot");
        let tools = snapshot
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                StoreError::InvalidData("catalog snapshot tools are malformed".to_owned())
            })?;
        for binding in &workflow.tool_bindings {
            if binding.catalog_epoch_id.as_uuid() != &current {
                return Err(StoreError::CatalogScopeViolation);
            }
            let matches = tools.iter().any(|tool| {
                tool.get("qualified_name").and_then(Value::as_str) == Some(&binding.qualified_name)
                    && tool.get("descriptor_digest").and_then(Value::as_str)
                        == Some(&binding.descriptor_digest)
            });
            if !matches {
                return Err(StoreError::CatalogScopeViolation);
            }
        }
        Ok(())
    }
}

fn catalog_json(epoch: &ToolCatalogSnapshot) -> Value {
    json!({"tools": epoch.tools().values().map(|tool| json!({"qualified_name": tool.qualified_name(), "descriptor_digest": tool.descriptor_digest(), "descriptor": tool.normalized_descriptor()})).collect::<Vec<_>>()})
}
fn bindings_json(workflow: &CompiledWorkflow) -> Value {
    json!(workflow.tool_bindings.iter().map(|binding| json!({"qualified_name": binding.qualified_name, "catalog_epoch_id": binding.catalog_epoch_id, "descriptor_digest": binding.descriptor_digest})).collect::<Vec<_>>())
}
fn definition_from_row(row: &sqlx::postgres::PgRow) -> PublishedDefinition {
    PublishedDefinition {
        id: row.get("id"),
        name: row.get("name"),
        version: row.get::<i32, _>("version") as u32,
        definition_hash: row.get("definition_hash"),
    }
}

fn run_state(value: &str) -> Result<RunState, StoreError> {
    match value {
        "runnable" => Ok(RunState::Runnable),
        "running" => Ok(RunState::Running),
        "waiting_for_answer" => Ok(RunState::WaitingForAnswer),
        "waiting_for_permission" => Ok(RunState::WaitingForPermission),
        "completed" => Ok(RunState::Completed),
        "failed" => Ok(RunState::Failed),
        "cancelled" => Ok(RunState::Cancelled),
        _ => Err(StoreError::InvalidData("run state is malformed".to_owned())),
    }
}

async fn append_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: uuid::Uuid,
    kind: &str,
    metadata: Value,
) -> Result<(), StoreError> {
    let sequence = sqlx::query("SELECT COALESCE(MAX(sequence), 0) + 1 AS sequence FROM events WHERE run_id = $1")
        .bind(run_id)
        .fetch_one(&mut **tx)
        .await?
        .get::<i64, _>("sequence");
    sqlx::query("INSERT INTO events (run_id, sequence, kind, metadata) VALUES ($1, $2, $3, $4)")
        .bind(run_id).bind(sequence).bind(kind).bind(metadata).execute(&mut **tx).await?;
    Ok(())
}
