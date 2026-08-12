use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

use crate::dsl::CompiledWorkflow;
use crate::{
    CatalogEpochId, CatalogTool, PolicyRevision, PrincipalId, SessionId, ToolCatalogSnapshot,
    canonical_sha256,
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
        let session_id = SessionId::new(id);
        let catalog = ToolCatalogSnapshot::new(session_id.clone(), uuid::Uuid::now_v7(), [])
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let snapshot = catalog_json(&catalog);
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO sessions (id, principal, working_directory, retention_until) VALUES ($1, $2, $3, $4)")
            .bind(id).bind(request.principal.as_str()).bind(request.working_directory).bind(request.retention_until)
            .execute(&mut *tx).await?;
        sqlx::query("INSERT INTO mcp_catalog_epochs (epoch_id, session_id, server_alias, refresh_identity, canonical_digest, connection_binding, descriptor_snapshot) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(*catalog.epoch_id().as_uuid()).bind(id).bind("session_initial").bind(catalog.epoch_id().as_uuid().to_string()).bind(catalog.digest()).bind(json!({"kind":"session_initial"})).bind(snapshot)
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE sessions SET current_catalog_epoch = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(*catalog.epoch_id().as_uuid()).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(SessionView {
            id: session_id,
            principal: request.principal,
            policy_revision: PolicyRevision::new(0),
            current_catalog_epoch: Some(catalog.epoch_id().clone()),
            mcp_attached: true,
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

    pub async fn list_sessions(&self, principal: &PrincipalId) -> Result<Vec<SessionView>, StoreError> {
        let rows = sqlx::query("SELECT id, principal, current_policy_revision, current_catalog_epoch FROM sessions WHERE principal = $1 AND state = 'active' ORDER BY updated_at DESC, id")
            .bind(principal.as_str()).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|row| SessionView {
            id: SessionId::new(row.get("id")), principal: principal.clone(), policy_revision: PolicyRevision::new(row.get::<i64, _>("current_policy_revision") as u64),
            current_catalog_epoch: row.get::<Option<uuid::Uuid>, _>("current_catalog_epoch").map(CatalogEpochId::new), mcp_attached: row.get::<Option<uuid::Uuid>, _>("current_catalog_epoch").is_some(),
        }).collect())
    }

    pub async fn close_session(&self, session_id: SessionId, principal: &PrincipalId) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE sessions SET state = 'closed', updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND principal = $2 AND state = 'active'")
            .bind(*session_id.as_uuid()).bind(principal.as_str()).execute(&self.pool).await?;
        if result.rows_affected() == 1 { return Ok(()); }
        let row = sqlx::query("SELECT principal, state FROM sessions WHERE id = $1").bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?.ok_or(StoreError::SessionNotFound)?;
        if row.get::<String, _>("principal") != principal.as_str() { return Err(StoreError::PrincipalMismatch); }
        Err(StoreError::SessionClosed)
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

    pub async fn current_catalog(&self, session_id: SessionId) -> Result<ToolCatalogSnapshot, StoreError> {
        let row = sqlx::query("SELECT s.current_catalog_epoch, e.descriptor_snapshot FROM sessions s LEFT JOIN mcp_catalog_epochs e ON e.epoch_id = s.current_catalog_epoch AND e.session_id = s.id WHERE s.id = $1")
            .bind(*session_id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        let epoch_id = row
            .get::<Option<uuid::Uuid>, _>("current_catalog_epoch")
            .ok_or(StoreError::CatalogScopeViolation)?;
        let snapshot = row
            .get::<Option<Value>, _>("descriptor_snapshot")
            .ok_or(StoreError::CatalogScopeViolation)?;
        let tools = snapshot
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| StoreError::InvalidData("catalog snapshot tools are malformed".to_owned()))?
            .iter()
            .map(|entry| {
                let descriptor = entry.get("descriptor").ok_or_else(|| {
                    StoreError::InvalidData("catalog entry descriptor is malformed".to_owned())
                })?;
                let tool = CatalogTool::from_normalized(descriptor)
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                if entry.get("qualified_name").and_then(Value::as_str) != Some(tool.qualified_name())
                    || entry.get("descriptor_digest").and_then(Value::as_str)
                        != Some(tool.descriptor_digest())
                {
                    return Err(StoreError::CatalogScopeViolation);
                }
                Ok(tool)
            })
            .collect::<Result<Vec<_>, _>>()
            ?;
        ToolCatalogSnapshot::new(session_id, epoch_id, tools)
            .map_err(|error| StoreError::InvalidData(error.to_string()))
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

    pub async fn current_session_policy(
        &self,
        session: SessionId,
    ) -> Result<crate::SessionPolicy, StoreError> {
        Ok(self.current_session_policy_snapshot(session).await?.0)
    }

    pub async fn current_session_policy_snapshot(
        &self,
        session: SessionId,
    ) -> Result<(crate::SessionPolicy, PolicyRevision), StoreError> {
        let row = sqlx::query(
            "SELECT s.current_policy_revision, p.typed_source \
             FROM sessions s \
             LEFT JOIN LATERAL ( \
               SELECT typed_source FROM session_policy_rules \
               WHERE session_id = s.id ORDER BY revision DESC LIMIT 1 \
             ) p ON TRUE WHERE s.id = $1",
        )
            .bind(*session.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        let policy = match row.get::<Option<Value>, _>("typed_source") {
            Some(source) => serde_json::from_value(source)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
            None => crate::SessionPolicy {
                version: 1,
                rules: vec![],
            },
        };
        Ok((
            policy,
            PolicyRevision::new(row.get::<i64, _>("current_policy_revision") as u64),
        ))
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
            sqlx::query("INSERT INTO run_tool_bindings (run_id, qualified_name, server_alias, catalog_epoch_id, descriptor_digest, capabilities, recovery_profile, descriptor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
                .bind(run_id).bind(qualified_name).bind(tool.get("server_alias").and_then(Value::as_str).ok_or_else(|| StoreError::InvalidData("catalog tool server alias is malformed".to_owned()))?).bind(catalog_epoch).bind(descriptor_digest).bind(tool.get("capabilities").cloned().ok_or_else(|| StoreError::InvalidData("catalog tool capabilities are malformed".to_owned()))?).bind(tool.get("recovery").and_then(Value::as_str).ok_or_else(|| StoreError::InvalidData("catalog tool recovery profile is malformed".to_owned()))?).bind(tool.clone()).execute(&mut *tx).await?;
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

    pub async fn runnable_frames(&self, limit: i64) -> Result<Vec<super::RunnableFrame>, StoreError> {
        self.runnable_frames_query("f.state = 'runnable' AND r.state IN ('runnable', 'running')", None, limit).await
    }

    pub async fn get_run(&self, session_id: crate::SessionId, run_id: crate::RunId) -> Result<super::RunView, StoreError> {
        let row = sqlx::query("SELECT r.id, r.session_id, r.state, r.revision, d.id AS definition_id, d.name AS definition_name, d.version AS definition_version, d.definition_hash FROM runs r JOIN workflow_definitions d ON d.id = r.definition_id WHERE r.id = $1 AND r.session_id = $2")
            .bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        let root = sqlx::query("SELECT id FROM frames WHERE run_id = $1 AND parent_frame_id IS NULL")
            .bind(*run_id.as_uuid()).fetch_one(&self.pool).await?;
        Ok(super::RunView {
            id: crate::RunId::new(row.get("id")), session_id: crate::SessionId::new(row.get("session_id")), root_frame_id: crate::FrameId::new(root.get("id")),
            definition: PublishedDefinition { id: row.get("definition_id"), name: row.get("definition_name"), version: row.get::<i32, _>("definition_version") as u32, definition_hash: row.get("definition_hash") },
            state: run_state(&row.get::<String, _>("state"))?, revision: row.get::<i64, _>("revision") as u64,
        })
    }

    pub async fn runnable_frames_for_run(&self, run_id: crate::RunId, limit: i64) -> Result<Vec<super::RunnableFrame>, StoreError> {
        self.runnable_frames_query("f.state = 'runnable' AND r.state IN ('runnable', 'running') AND f.run_id = $1", Some(run_id), limit).await
    }

    async fn runnable_frames_query(&self, predicate: &str, run_id: Option<crate::RunId>, limit: i64) -> Result<Vec<super::RunnableFrame>, StoreError> {
        let sql = format!("SELECT f.id, f.run_id, r.session_id, f.task_name, f.outcome_name, f.statement_index, f.bindings, f.revision, d.compiled_definition FROM frames f JOIN runs r ON r.id = f.run_id JOIN workflow_definitions d ON d.id = r.definition_id WHERE {predicate} ORDER BY f.created_at, f.id LIMIT {}", if run_id.is_some() { "$2" } else { "$1" });
        let mut query = sqlx::query(&sql);
        if let Some(run_id) = run_id { query = query.bind(*run_id.as_uuid()); }
        let rows = query.bind(limit.clamp(1, 256)).fetch_all(&self.pool).await?;
        rows.into_iter().map(|row| Ok(super::RunnableFrame {
            id: crate::FrameId::new(row.get("id")), run_id: crate::RunId::new(row.get("run_id")), session_id: crate::SessionId::new(row.get("session_id")), task_name: row.get("task_name"), outcome_name: row.get("outcome_name"), statement_index: row.get::<i32, _>("statement_index") as usize, bindings: row.get("bindings"), revision: row.get::<i64, _>("revision") as u64,
            workflow: serde_json::from_value(row.get("compiled_definition")).map_err(|error| StoreError::InvalidData(error.to_string()))?,
        })).collect()
    }

    pub async fn run_tool_binding(&self, run_id: crate::RunId, qualified_name: &str) -> Result<super::RunToolBinding, StoreError> {
        let row = sqlx::query("SELECT qualified_name, descriptor_digest, capabilities, descriptor FROM run_tool_bindings WHERE run_id = $1 AND qualified_name = $2")
            .bind(*run_id.as_uuid()).bind(qualified_name).fetch_optional(&self.pool).await?
            .ok_or(StoreError::CatalogScopeViolation)?;
        let capabilities = serde_json::from_value(row.get("capabilities")).map_err(|error| StoreError::InvalidData(error.to_string()))?;
        Ok(super::RunToolBinding { qualified_name: row.get("qualified_name"), descriptor_digest: row.get("descriptor_digest"), capabilities, descriptor: row.get::<Option<Value>, _>("descriptor").ok_or_else(|| StoreError::InvalidData("run tool descriptor is absent".to_owned()))? })
    }

    pub async fn run_tool_bindings(&self, run_id: crate::RunId) -> Result<Vec<super::RunToolBinding>, StoreError> {
        let rows = sqlx::query("SELECT qualified_name, descriptor_digest, capabilities, descriptor FROM run_tool_bindings WHERE run_id = $1 ORDER BY qualified_name")
            .bind(*run_id.as_uuid()).fetch_all(&self.pool).await?;
        rows.into_iter().map(|row| {
            let capabilities = serde_json::from_value(row.get("capabilities")).map_err(|error| StoreError::InvalidData(error.to_string()))?;
            let descriptor = row.get::<Option<Value>, _>("descriptor").ok_or_else(|| StoreError::InvalidData("run tool descriptor is absent".to_owned()))?;
            Ok(super::RunToolBinding { qualified_name: row.get("qualified_name"), descriptor_digest: row.get("descriptor_digest"), capabilities, descriptor })
        }).collect()
    }

    pub async fn policy_revision(&self, session_id: crate::SessionId) -> Result<crate::PolicyRevision, StoreError> {
        let row = sqlx::query("SELECT current_policy_revision FROM sessions WHERE id = $1")
            .bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(crate::PolicyRevision::new(row.get::<i64, _>("current_policy_revision") as u64))
    }

    pub async fn frame_history(&self, frame_id: crate::FrameId) -> Result<Vec<Value>, StoreError> {
        let mut items = sqlx::query("SELECT item FROM model_items WHERE frame_id = $1 AND direction = 'output' ORDER BY attempt, position")
            .bind(*frame_id.as_uuid()).fetch_all(&self.pool).await?
            .into_iter().map(|row| row.get("item")).collect::<Vec<Value>>();
        let calls = sqlx::query("SELECT provider_call_id, result, error FROM tool_calls WHERE frame_id = $1 AND state IN ('completed', 'failed') ORDER BY created_at, id")
            .bind(*frame_id.as_uuid()).fetch_all(&self.pool).await?;
        items.extend(calls.into_iter().map(|row| json!({
            "type": "function_call_output", "call_id": row.get::<String, _>("provider_call_id"),
            "output": row.get::<Option<Value>, _>("result").or_else(|| row.get::<Option<Value>, _>("error")).unwrap_or(Value::Null),
        })));
        Ok(items)
    }

    pub async fn model_attempt_count(&self, frame_id: crate::FrameId) -> Result<u64, StoreError> {
        let row = sqlx::query("SELECT COUNT(DISTINCT attempt) AS count FROM model_items WHERE frame_id = $1")
            .bind(*frame_id.as_uuid()).fetch_one(&self.pool).await?;
        Ok(row.get::<i64, _>("count") as u64)
    }

    pub async fn permission_suspension(&self, session_id: crate::SessionId, run_id: crate::RunId, request_id: crate::PermissionRequestId) -> Result<super::SuspensionFrame, StoreError> {
        let row = sqlx::query("SELECT f.id, f.revision FROM permission_requests p JOIN frames f ON f.id = p.frame_id JOIN runs r ON r.id = p.run_id WHERE p.id = $1 AND p.run_id = $2 AND r.session_id = $3 AND p.state IN ('pending', 'resolved')")
            .bind(*request_id.as_uuid()).bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(super::SuspensionFrame { frame_id: crate::FrameId::new(row.get("id")), revision: row.get::<i64, _>("revision") as u64 })
    }

    pub async fn pending_permission(&self, session_id: crate::SessionId, run_id: crate::RunId, request_id: crate::PermissionRequestId) -> Result<super::PendingPermission, StoreError> {
        let row = sqlx::query("SELECT f.id, f.revision, t.id AS tool_call_id, p.provider_call_id, p.qualified_tool, p.descriptor_digest, t.arguments, p.state, p.resolution_idempotency_key FROM permission_requests p JOIN frames f ON f.id = p.frame_id JOIN runs r ON r.id = p.run_id JOIN tool_calls t ON t.frame_id = p.frame_id AND t.provider_call_id = p.provider_call_id WHERE p.id = $1 AND p.run_id = $2 AND r.session_id = $3 AND p.state IN ('pending', 'resolved')")
            .bind(*request_id.as_uuid()).bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(super::PendingPermission { tool_call_id: row.get("tool_call_id"), frame_id: crate::FrameId::new(row.get("id")), revision: row.get::<i64, _>("revision") as u64, provider_call_id: row.get("provider_call_id"), qualified_name: row.get("qualified_tool"), descriptor_digest: row.get("descriptor_digest"), arguments: row.get("arguments"), resolved: row.get::<String, _>("state") == "resolved", resolution_idempotency_key: row.get("resolution_idempotency_key") })
    }

    pub async fn question_suspension(&self, session_id: crate::SessionId, run_id: crate::RunId, question_id: uuid::Uuid) -> Result<super::SuspensionFrame, StoreError> {
        let row = sqlx::query("SELECT f.id, f.revision FROM questions q JOIN frames f ON f.id = q.frame_id JOIN runs r ON r.id = q.run_id WHERE q.id = $1 AND q.run_id = $2 AND r.session_id = $3 AND q.state IN ('pending', 'answered')")
            .bind(question_id).bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(super::SuspensionFrame { frame_id: crate::FrameId::new(row.get("id")), revision: row.get::<i64, _>("revision") as u64 })
    }

    pub async fn run_revision(&self, session_id: crate::SessionId, run_id: crate::RunId) -> Result<u64, StoreError> {
        let row = sqlx::query("SELECT revision FROM runs WHERE id = $1 AND session_id = $2")
            .bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(row.get::<i64, _>("revision") as u64)
    }

    pub async fn events_after(&self, session_id: crate::SessionId, run_id: crate::RunId, after: u64, limit: u32) -> Result<Vec<super::RunEvent>, StoreError> {
        let rows = sqlx::query("SELECT e.sequence, e.kind, e.metadata, e.created_at FROM events e JOIN runs r ON r.id = e.run_id WHERE e.run_id = $1 AND r.session_id = $2 AND e.sequence > $3 ORDER BY e.sequence LIMIT $4")
            .bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).bind(after as i64).bind(i64::from(limit.clamp(1, 256))).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|row| super::RunEvent { sequence: row.get::<i64, _>("sequence") as u64, kind: row.get("kind"), metadata: row.get("metadata"), created_at: row.get("created_at") }).collect())
    }

    pub async fn requeue_uncommitted_model_attempts(&self, limit: i64) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let limit = limit.clamp(1, 256);
        let run_ids = sqlx::query_scalar::<_, uuid::Uuid>("SELECT r.id FROM runs r WHERE EXISTS (SELECT 1 FROM frames f WHERE f.run_id = r.id AND f.state = 'running' AND EXISTS (SELECT 1 FROM model_items m WHERE m.frame_id = f.id AND m.direction = 'input') AND NOT EXISTS (SELECT 1 FROM tool_calls t WHERE t.frame_id = f.id AND t.state IN ('intent', 'running', 'waiting_for_permission')) AND NOT EXISTS (SELECT 1 FROM model_items m WHERE m.frame_id = f.id AND m.direction = 'output')) ORDER BY r.id LIMIT $1 FOR UPDATE OF r SKIP LOCKED")
            .bind(limit)
            .fetch_all(&mut *tx)
            .await?;
        let frames = sqlx::query("SELECT f.id, f.run_id FROM frames f WHERE f.run_id = ANY($1) AND f.state = 'running' AND EXISTS (SELECT 1 FROM model_items m WHERE m.frame_id = f.id AND m.direction = 'input') AND NOT EXISTS (SELECT 1 FROM tool_calls t WHERE t.frame_id = f.id AND t.state IN ('intent', 'running', 'waiting_for_permission')) AND NOT EXISTS (SELECT 1 FROM model_items m WHERE m.frame_id = f.id AND m.direction = 'output') ORDER BY f.id LIMIT $2 FOR UPDATE OF f SKIP LOCKED")
            .bind(&run_ids)
            .bind(limit).fetch_all(&mut *tx).await?;
        for frame in &frames {
            let frame_id = frame.get::<uuid::Uuid, _>("id");
            let run_id = frame.get::<uuid::Uuid, _>("run_id");
            sqlx::query("UPDATE frames SET state = 'runnable', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
                .bind(frame_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE runs SET state = 'runnable', updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND state = 'running'")
                .bind(run_id).execute(&mut *tx).await?;
            append_event(&mut tx, run_id, "model.requeued", json!({"frame_id": frame_id})).await?;
        }
        tx.commit().await?;
        Ok(frames.len() as u64)
    }

    pub async fn recoverable_tool_calls(
        &self,
        limit: i64,
    ) -> Result<Vec<super::RecoverableToolCall>, StoreError> {
        self.recoverable_tool_calls_excluding(limit, &std::collections::BTreeSet::new()).await
    }

    pub async fn recoverable_tool_calls_excluding(
        &self,
        limit: i64,
        excluded_sessions: &std::collections::BTreeSet<crate::SessionId>,
    ) -> Result<Vec<super::RecoverableToolCall>, StoreError> {
        let excluded_sessions = excluded_sessions.iter().map(|session| *session.as_uuid()).collect::<Vec<_>>();
        let rows = sqlx::query("SELECT t.id, t.frame_id, r.id AS run_id, r.session_id, f.revision, t.qualified_name, t.descriptor_digest, t.idempotency_key, t.arguments, b.recovery_profile FROM tool_calls t JOIN frames f ON f.id = t.frame_id JOIN runs r ON r.id = f.run_id JOIN run_tool_bindings b ON b.run_id = r.id AND b.qualified_name = t.qualified_name AND b.descriptor_digest = t.descriptor_digest WHERE t.state = 'intent' AND f.state = 'running' AND r.state = 'running' AND NOT (r.session_id = ANY($2)) ORDER BY t.updated_at, t.id LIMIT $1")
            .bind(limit.clamp(1, 256))
            .bind(excluded_sessions)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|row| {
            Ok(super::RecoverableToolCall {
                id: row.get("id"), frame_id: crate::FrameId::new(row.get("frame_id")),
                run_id: crate::RunId::new(row.get("run_id")), session_id: crate::SessionId::new(row.get("session_id")),
                revision: row.get::<i64, _>("revision") as u64, qualified_name: row.get("qualified_name"),
                descriptor_digest: row.get("descriptor_digest"), idempotency_key: crate::IdempotencyKey::new(row.get::<String, _>("idempotency_key")).map_err(|error| StoreError::InvalidData(error.to_string()))?,
                arguments: row.get("arguments"),
                recovery: serde_json::from_value(Value::String(row.get("recovery_profile"))).map_err(|error| StoreError::InvalidData(error.to_string()))?,
            })
        }).collect()
    }

    pub async fn begin_tool_status_check(
        &self,
        frame_id: crate::FrameId,
        tool_call_id: uuid::Uuid,
        expected_revision: u64,
        retry_budget: u32,
    ) -> Result<Option<u64>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision || frame.get::<String, _>("state") != "running" {
            return Err(StoreError::RevisionConflict);
        }
        let call = sqlx::query("SELECT state, recovery_attempts FROM tool_calls WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(tool_call_id).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("tool call was not found for frame".to_owned()))?;
        if call.get::<String, _>("state") != "intent" {
            return Err(StoreError::RevisionConflict);
        }
        if call.get::<i32, _>("recovery_attempts") as u32 >= retry_budget {
            tx.commit().await?;
            return Ok(None);
        }
        let revision = expected_revision + 1;
        sqlx::query("UPDATE tool_calls SET recovery_attempts = recovery_attempts + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(tool_call_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        append_event(
            &mut tx,
            run_id,
            "tool.status_check_intent",
            json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id}),
        ).await?;
        tx.commit().await?;
        Ok(Some(revision))
    }

    pub async fn claim_tool_recovery_retry(
        &self,
        frame_id: crate::FrameId,
        tool_call_id: uuid::Uuid,
        expected_revision: u64,
        retry_budget: u32,
    ) -> Result<Option<u64>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision || frame.get::<String, _>("state") != "running" {
            return Err(StoreError::RevisionConflict);
        }
        let call = sqlx::query("SELECT state, recovery_attempts FROM tool_calls WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(tool_call_id).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("tool call was not found for frame".to_owned()))?;
        if call.get::<String, _>("state") != "intent" {
            return Err(StoreError::RevisionConflict);
        }
        if call.get::<i32, _>("recovery_attempts") as u32 >= retry_budget {
            tx.commit().await?;
            return Ok(None);
        }
        let revision = expected_revision + 1;
        sqlx::query("UPDATE tool_calls SET recovery_attempts = recovery_attempts + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(tool_call_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "tool.recovery_retry", json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id})).await?;
        tx.commit().await?;
        Ok(Some(revision))
    }

    pub async fn mark_tool_call_outcome_unknown(
        &self,
        frame_id: crate::FrameId,
        tool_call_id: uuid::Uuid,
        expected_revision: u64,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision || frame.get::<String, _>("state") != "running" {
            return Err(StoreError::RevisionConflict);
        }
        let call = sqlx::query("SELECT state FROM tool_calls WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(tool_call_id).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("tool call was not found for frame".to_owned()))?;
        if call.get::<String, _>("state") != "intent" {
            return Err(StoreError::RevisionConflict);
        }
        sqlx::query("UPDATE tool_calls SET state = 'outcome_unknown', updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(tool_call_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE frames SET state = 'outcome_unknown', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'outcome_unknown', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(run_id).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "tool.outcome_unknown", json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id})).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn begin_model_attempt(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        input: Value,
    ) -> Result<super::ModelAttempt, StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
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
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
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

    pub async fn record_completion(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        outcome_name: String,
        fields: Value,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state, bindings FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision || !matches!(frame.get::<String, _>("state").as_str(), "running" | "runnable") {
            return Err(StoreError::RevisionConflict);
        }
        let mut bindings = frame.get::<Value, _>("bindings").as_object().cloned().ok_or_else(|| StoreError::InvalidData("frame bindings are malformed".to_owned()))?;
        let fields = fields.as_object().ok_or_else(|| StoreError::InvalidData("completion fields must be an object".to_owned()))?;
        bindings.extend(fields.clone());
        sqlx::query("UPDATE frames SET state = 'runnable', outcome_name = $1, statement_index = 0, bindings = $2, revision = $3, updated_at = CURRENT_TIMESTAMP WHERE id = $4")
            .bind(&outcome_name).bind(Value::Object(bindings)).bind((expected_revision + 1) as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "model.completed_task", json!({"frame_id": frame_id.as_uuid(), "outcome": outcome_name})).await?;
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
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::FrameNotFound)?;
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
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
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
        sqlx::query("UPDATE frames SET state = 'runnable', revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
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
        let ownership = sqlx::query("SELECT f.run_id, r.session_id FROM frames f JOIN runs r ON r.id = f.run_id WHERE f.id = $1")
            .bind(*frame_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::FrameNotFound)?;
        let run_id: uuid::Uuid = ownership.get("run_id");
        let session_id: uuid::Uuid = ownership.get("session_id");
        let policy =
            sqlx::query("SELECT current_policy_revision FROM sessions WHERE id = $1 FOR UPDATE")
                .bind(session_id)
                .fetch_one(&mut *tx)
                .await?;
        lock_run(&mut tx, run_id).await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
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
    ) -> Result<bool, StoreError> {
        self.resolve_permission_with_policy(
            frame_id,
            request_id,
            expected_revision,
            resolution,
            idempotency_key,
            None,
        )
        .await
    }

    pub async fn resolve_permission_with_policy(
        &self,
        frame_id: crate::FrameId,
        request_id: crate::PermissionRequestId,
        expected_revision: u64,
        resolution: Value,
        idempotency_key: String,
        policy_update: Option<PolicyUpdate>,
    ) -> Result<bool, StoreError> {
        let aggregate = sqlx::query(
            "SELECT r.id AS run_id, r.session_id FROM frames f \
             JOIN runs r ON r.id = f.run_id WHERE f.id = $1",
        )
        .bind(*frame_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::FrameNotFound)?;
        let run_id: uuid::Uuid = aggregate.get("run_id");
        let session_id: uuid::Uuid = aggregate.get("session_id");
        let mut tx = self.pool.begin().await?;
        let session = sqlx::query(
            "SELECT current_policy_revision, state FROM sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::SessionNotFound)?;
        if session.get::<String, _>("state") != "active" {
            return Err(StoreError::SessionClosed);
        }
        lock_run(&mut tx, run_id).await?;
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<uuid::Uuid, _>("run_id") != run_id {
            return Err(StoreError::RevisionConflict);
        }
        let request = sqlx::query("SELECT state, resolution_idempotency_key FROM permission_requests WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(*request_id.as_uuid()).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("permission request was not found for frame".to_owned()))?;
        if request.get::<String, _>("state") == "resolved" {
            if request.get::<Option<String>, _>("resolution_idempotency_key") == Some(idempotency_key) {
                return Ok(false);
            }
            return Err(StoreError::IdempotencyConflict);
        }
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "waiting_for_permission"
            || request.get::<String, _>("state") != "pending"
        {
            return Err(StoreError::RevisionConflict);
        }
        if let Some(update) = policy_update {
            if *update.session.as_uuid() != session_id {
                return Err(StoreError::CatalogScopeViolation);
            }
            let digest = canonical_sha256(&update.typed_source)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?;
            if let Some(existing) = sqlx::query("SELECT source_digest, revision FROM session_policy_rules WHERE session_id = $1 AND idempotency_key = $2")
                .bind(*update.session.as_uuid()).bind(&update.idempotency_key).fetch_optional(&mut *tx).await?
            {
                if existing.get::<String, _>("source_digest") != digest {
                    return Err(StoreError::IdempotencyConflict);
                }
            } else {
                let current = session.get::<i64, _>("current_policy_revision") as u64;
                if current != update.expected_revision.get() {
                    return Err(StoreError::PolicyRevisionConflict);
                }
                let revision = current + 1;
                sqlx::query("INSERT INTO session_policy_rules (id, session_id, revision, idempotency_key, typed_source, compiled_cedar, source_digest) VALUES ($1, $2, $3, $4, $5, $6, $7)")
                    .bind(uuid::Uuid::now_v7()).bind(*update.session.as_uuid()).bind(revision as i64).bind(update.idempotency_key).bind(update.typed_source).bind(update.compiled_cedar).bind(digest).execute(&mut *tx).await?;
                sqlx::query("UPDATE sessions SET current_policy_revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(revision as i64).bind(*update.session.as_uuid()).execute(&mut *tx).await?;
            }
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
        Ok(true)
    }

    pub async fn create_question(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        question: super::Question,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision || !matches!(frame.get::<String, _>("state").as_str(), "running" | "runnable") {
            return Err(StoreError::RevisionConflict);
        }
        question.schema.validate_definition().map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let schema = serde_json::to_value(&question.schema).map_err(|error| StoreError::InvalidData(error.to_string()))?;
        sqlx::query("INSERT INTO questions (id, run_id, frame_id, binding, text, schema, state) VALUES ($1, $2, $3, $4, $5, $6, 'pending')")
            .bind(question.id).bind(run_id).bind(*frame_id.as_uuid()).bind(&question.binding).bind(&question.text).bind(schema).execute(&mut *tx).await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'waiting_for_answer', suspension_id = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
            .bind(question.id).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'waiting_for_answer', updated_at = CURRENT_TIMESTAMP WHERE id = $1").bind(run_id).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "question.requested", json!({"frame_id": frame_id.as_uuid(), "question_id": question.id, "binding": question.binding})).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn answer(
        &self,
        frame_id: crate::FrameId,
        question_id: uuid::Uuid,
        expected_revision: u64,
        answer: Value,
        idempotency_key: String,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT run_id, revision, state, bindings FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(StoreError::FrameNotFound)?;
        let question = sqlx::query("SELECT binding, schema, state, answer_idempotency_key FROM questions WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(question_id).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("question was not found for frame".to_owned()))?;
        if question.get::<String, _>("state") == "answered" {
            if question.get::<Option<String>, _>("answer_idempotency_key") == Some(idempotency_key) { return Ok(()); }
            return Err(StoreError::IdempotencyConflict);
        }
        if frame.get::<i64, _>("revision") as u64 != expected_revision || frame.get::<String, _>("state") != "waiting_for_answer" || question.get::<String, _>("state") != "pending" {
            return Err(StoreError::RevisionConflict);
        }
        let schema: crate::ClosedSchema = serde_json::from_value(question.get("schema")).map_err(|error| StoreError::InvalidData(error.to_string()))?;
        schema.validate(&answer).map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let mut bindings = frame.get::<Value, _>("bindings").as_object().cloned().ok_or_else(|| StoreError::InvalidData("frame bindings are malformed".to_owned()))?;
        bindings.insert(question.get("binding"), answer.clone());
        sqlx::query("UPDATE questions SET state = 'answered', answer = $1, answer_idempotency_key = $2, answered_at = CURRENT_TIMESTAMP WHERE id = $3")
            .bind(answer).bind(idempotency_key).bind(question_id).execute(&mut *tx).await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'runnable', suspension_id = NULL, bindings = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
            .bind(Value::Object(bindings)).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'runnable', updated_at = CURRENT_TIMESTAMP WHERE id = $1").bind(run_id).execute(&mut *tx).await?;
        append_event(&mut tx, run_id, "question.answered", json!({"frame_id": frame_id.as_uuid(), "question_id": question_id})).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn apply_frame_command(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        command: super::FrameCommand,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let metadata = sqlx::query("SELECT run_id, parent_frame_id FROM frames WHERE id = $1")
            .bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or(StoreError::FrameNotFound)?;
        let run_id: uuid::Uuid = metadata.get("run_id");
        lock_run(&mut tx, run_id).await?;
        let parent_id = matches!(&command, super::FrameCommand::Complete(_))
            .then(|| metadata.get::<Option<uuid::Uuid>, _>("parent_frame_id"))
            .flatten();
        let mut frame_ids = vec![*frame_id.as_uuid()];
        frame_ids.extend(parent_id);
        frame_ids.sort_unstable();
        frame_ids.dedup();
        let mut locked_frames = Vec::with_capacity(frame_ids.len());
        for locked_frame_id in frame_ids {
            let frame = sqlx::query("SELECT id, run_id, parent_frame_id, parent_binding, bindings, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(locked_frame_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
            locked_frames.push(frame);
        }
        let frame = locked_frames
            .iter()
            .find(|frame| frame.get::<uuid::Uuid, _>("id") == *frame_id.as_uuid()).ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision || !matches!(frame.get::<String, _>("state").as_str(), "running" | "runnable") {
            return Err(StoreError::RevisionConflict);
        }
        let revision = expected_revision + 1;
        match command {
            super::FrameCommand::StartCall(child) => {
                insert_child_frame(&mut tx, run_id, *frame_id.as_uuid(), child).await?;
                sqlx::query("UPDATE frames SET state = 'waiting_for_children', statement_index = statement_index + 1, revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "frame.call_started", json!({"frame_id": frame_id.as_uuid()})).await?;
            }
            super::FrameCommand::StartParallel(children) => {
                if children.is_empty() { return Err(StoreError::InvalidData("parallel command has no children".to_owned())); }
                for child in children { insert_child_frame(&mut tx, run_id, *frame_id.as_uuid(), child).await?; }
                sqlx::query("UPDATE frames SET state = 'waiting_for_children', statement_index = statement_index + 1, revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "frame.parallel_started", json!({"frame_id": frame_id.as_uuid()})).await?;
            }
            super::FrameCommand::SuspendAsk(question) => {
                question.schema.validate_definition().map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let schema = serde_json::to_value(&question.schema).map_err(|error| StoreError::InvalidData(error.to_string()))?;
                sqlx::query("INSERT INTO questions (id, run_id, frame_id, binding, text, schema, state) VALUES ($1, $2, $3, $4, $5, $6, 'pending')")
                    .bind(question.id).bind(run_id).bind(*frame_id.as_uuid()).bind(&question.binding).bind(&question.text).bind(schema).execute(&mut *tx).await?;
                sqlx::query("UPDATE frames SET state = 'waiting_for_answer', suspension_id = $1, statement_index = statement_index + 1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
                    .bind(question.id).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                sqlx::query("UPDATE runs SET state = 'waiting_for_answer', updated_at = CURRENT_TIMESTAMP WHERE id = $1").bind(run_id).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "question.requested", json!({"frame_id": frame_id.as_uuid(), "question_id": question.id, "binding": question.binding})).await?;
            }
            super::FrameCommand::Goto { task_name, bindings } => {
                sqlx::query("UPDATE frames SET task_name = $1, bindings = $2, state = 'runnable', revision = $3, updated_at = CURRENT_TIMESTAMP WHERE id = $4")
                    .bind(task_name).bind(serde_json::to_value(bindings).map_err(|error| StoreError::InvalidData(error.to_string()))?).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "frame.goto", json!({"frame_id": frame_id.as_uuid()})).await?;
            }
            super::FrameCommand::Complete(result) => {
                sqlx::query("UPDATE frames SET state = 'completed', result = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
                    .bind(result.clone()).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                if frame.get::<Option<uuid::Uuid>, _>("parent_frame_id").is_none() {
                    sqlx::query("UPDATE runs SET state = 'completed', output = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                        .bind(result).bind(run_id).execute(&mut *tx).await?;
                } else {
                    let parent = locked_frames
                        .iter()
                        .find(|parent| Some(parent.get::<uuid::Uuid, _>("id")) == parent_id)
                        .ok_or(StoreError::FrameNotFound)?;
                    resume_parent_after_child(&mut tx, run_id, frame, parent, result).await?;
                }
                append_event(&mut tx, run_id, "frame.completed", json!({"frame_id": frame_id.as_uuid()})).await?;
            }
            super::FrameCommand::Fail { code, message } => {
                let failure = json!({"code": code, "message": message});
                sqlx::query("UPDATE frames SET state = 'failed', failure = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
                    .bind(&failure).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                sqlx::query("UPDATE runs SET state = 'failed', failure = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(failure).bind(run_id).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "frame.failed", json!({"frame_id": frame_id.as_uuid()})).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn cancel_run(
        &self,
        run_id: crate::RunId,
        expected_revision: u64,
        reason: Option<String>,
        idempotency_key: String,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let run = sqlx::query("SELECT revision, state, cancellation_idempotency_key FROM runs WHERE id = $1 FOR UPDATE")
            .bind(*run_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("run was not found".to_owned()))?;
        if run.get::<String, _>("state") == "cancelled" {
            if run.get::<Option<String>, _>("cancellation_idempotency_key") == Some(idempotency_key) {
                return Ok(());
            }
            return Err(StoreError::IdempotencyConflict);
        }
        if run.get::<i64, _>("revision") as u64 != expected_revision || matches!(run.get::<String, _>("state").as_str(), "completed" | "failed") {
            return Err(StoreError::RevisionConflict);
        }
        sqlx::query("SELECT id FROM frames WHERE run_id = $1 AND state NOT IN ('completed', 'failed', 'cancelled') ORDER BY id FOR UPDATE")
            .bind(*run_id.as_uuid())
            .fetch_all(&mut *tx)
            .await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'cancelled', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE run_id = $1 AND state NOT IN ('completed', 'failed', 'cancelled')")
            .bind(*run_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'cancelled', revision = $1, cancellation_idempotency_key = $2, cancellation_reason = $3, updated_at = CURRENT_TIMESTAMP WHERE id = $4")
            .bind(revision as i64).bind(idempotency_key).bind(reason.clone().map(Value::String)).bind(*run_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE questions SET state = 'cancelled' WHERE run_id = $1 AND state = 'pending'")
            .bind(*run_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE permission_requests SET state = 'cancelled' WHERE run_id = $1 AND state = 'pending'")
            .bind(*run_id.as_uuid()).execute(&mut *tx).await?;
        append_event(&mut tx, *run_id.as_uuid(), "run.cancelled", json!({"reason": reason})).await?;
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
        "outcome_unknown" => Ok(RunState::OutcomeUnknown),
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

async fn lock_run_for_frame(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    frame_id: uuid::Uuid,
) -> Result<uuid::Uuid, StoreError> {
    // A frame's run is immutable, so discover it before taking the run-level
    // aggregate mutex. Every caller locks its frame only after this returns.
    let run_id = sqlx::query_scalar("SELECT run_id FROM frames WHERE id = $1")
        .bind(frame_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::FrameNotFound)?;
    lock_run(tx, run_id).await?;
    Ok(run_id)
}

async fn lock_run(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: uuid::Uuid,
) -> Result<(), StoreError> {
    sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE")
        .bind(run_id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}

async fn insert_child_frame(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: uuid::Uuid,
    parent_frame_id: uuid::Uuid,
    child: super::NewChild,
) -> Result<(), StoreError> {
    sqlx::query("INSERT INTO frames (id, run_id, parent_frame_id, parent_binding, task_name, branch_index, bindings, state) VALUES ($1, $2, $3, $4, $5, $6, $7, 'runnable')")
        .bind(uuid::Uuid::now_v7()).bind(run_id).bind(parent_frame_id).bind(child.binding).bind(child.target).bind(child.branch_index).bind(serde_json::to_value(child.arguments).map_err(|error| StoreError::InvalidData(error.to_string()))?).execute(&mut **tx).await?;
    Ok(())
}

async fn resume_parent_after_child(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: uuid::Uuid,
    child: &sqlx::postgres::PgRow,
    parent: &sqlx::postgres::PgRow,
    result: Value,
) -> Result<(), StoreError> {
    let parent_id = child.get::<Option<uuid::Uuid>, _>("parent_frame_id").ok_or_else(|| StoreError::InvalidData("child frame has no parent".to_owned()))?;
    let binding = child.get::<Option<String>, _>("parent_binding").ok_or_else(|| StoreError::InvalidData("child frame has no parent binding".to_owned()))?;
    let mut bindings = parent.get::<Value, _>("bindings").as_object().cloned().ok_or_else(|| StoreError::InvalidData("parent bindings are malformed".to_owned()))?;
    bindings.insert(binding, result);
    sqlx::query("UPDATE frames SET bindings = $1, revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
        .bind(Value::Object(bindings)).bind(parent_id).execute(&mut **tx).await?;
    let incomplete = sqlx::query("SELECT 1 FROM frames WHERE parent_frame_id = $1 AND state <> 'completed' LIMIT 1")
        .bind(parent_id).fetch_optional(&mut **tx).await?;
    if incomplete.is_none() {
        sqlx::query("UPDATE frames SET state = 'runnable', updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(parent_id).execute(&mut **tx).await?;
        append_event(tx, run_id, "frame.children_completed", json!({"parent_frame_id": parent_id})).await?;
    }
    Ok(())
}
