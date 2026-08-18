use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

use crate::dsl::{CompiledDefinition, DefinitionKind};
use crate::{
    CatalogEpochId, CatalogTool, GoalContract, PolicyRevision, PrincipalId, SessionId,
    SubmitUserTask, TaskPreferences, TaskTarget, ToolCatalogSnapshot, UserTaskId, canonical_sha256,
};

use super::{
    AcceptedUserTask, CreateSession, PolicyUpdate, PublishedDefinition, RunState, RunView,
    SessionView, StartRun, StoreError,
};

pub struct PostgresStore {
    pool: PgPool,
    limits: super::StoreLimits,
}

impl PostgresStore {
    pub async fn connect(database_url: &str) -> Result<Self, StoreError> {
        Self::connect_with_limits(database_url, super::StoreLimits::default()).await
    }

    pub async fn connect_with_limits(
        database_url: &str,
        limits: super::StoreLimits,
    ) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool, limits })
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
        lock_principal(&mut tx, request.principal.as_str()).await?;
        let active_sessions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sessions WHERE principal = $1 AND state = 'active'",
        )
        .bind(request.principal.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if active_sessions
            >= self
                .limits
                .max_active_sessions_per_principal
                .min(i64::MAX as usize) as i64
        {
            return Err(StoreError::QuotaExceeded("active_sessions_per_principal"));
        }
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
            mcp_attached: false,
        })
    }

    pub async fn resume_session(
        &self,
        id: SessionId,
        principal: &PrincipalId,
    ) -> Result<SessionView, StoreError> {
        let row = sqlx::query("SELECT s.principal, s.state, s.current_policy_revision, s.current_catalog_epoch, e.connection_binding FROM sessions s LEFT JOIN mcp_catalog_epochs e ON e.epoch_id = s.current_catalog_epoch AND e.session_id = s.id WHERE s.id = $1")
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
            mcp_attached: catalog_requires_mcp_attachment(
                row.get::<Option<Value>, _>("connection_binding").as_ref(),
            ),
            current_catalog_epoch: epoch,
        })
    }

    pub async fn list_sessions(
        &self,
        principal: &PrincipalId,
    ) -> Result<Vec<SessionView>, StoreError> {
        let rows = sqlx::query("SELECT s.id, s.principal, s.current_policy_revision, s.current_catalog_epoch, e.connection_binding FROM sessions s LEFT JOIN mcp_catalog_epochs e ON e.epoch_id = s.current_catalog_epoch AND e.session_id = s.id WHERE s.principal = $1 AND s.state = 'active' ORDER BY s.updated_at DESC, s.id")
            .bind(principal.as_str()).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|row| SessionView {
                id: SessionId::new(row.get("id")),
                principal: principal.clone(),
                policy_revision: PolicyRevision::new(
                    row.get::<i64, _>("current_policy_revision") as u64
                ),
                current_catalog_epoch: row
                    .get::<Option<uuid::Uuid>, _>("current_catalog_epoch")
                    .map(CatalogEpochId::new),
                mcp_attached: catalog_requires_mcp_attachment(
                    row.get::<Option<Value>, _>("connection_binding").as_ref(),
                ),
            })
            .collect())
    }

    pub async fn close_session(
        &self,
        session_id: SessionId,
        principal: &PrincipalId,
    ) -> Result<(), StoreError> {
        let result = sqlx::query("UPDATE sessions SET state = 'closed', updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND principal = $2 AND state = 'active'")
            .bind(*session_id.as_uuid()).bind(principal.as_str()).execute(&self.pool).await?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        let row = sqlx::query("SELECT principal, state FROM sessions WHERE id = $1")
            .bind(*session_id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        if row.get::<String, _>("principal") != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        Err(StoreError::SessionClosed)
    }

    pub async fn append_catalog_epoch(
        &self,
        epoch: &ToolCatalogSnapshot,
    ) -> Result<(), StoreError> {
        self.append_catalog_epoch_with_binding(epoch, "session_catalog")
            .await
    }

    pub async fn append_mcp_catalog_epoch(
        &self,
        epoch: &ToolCatalogSnapshot,
    ) -> Result<(), StoreError> {
        self.append_catalog_epoch_with_binding(epoch, "mcp_catalog")
            .await
    }

    async fn append_catalog_epoch_with_binding(
        &self,
        epoch: &ToolCatalogSnapshot,
        binding_kind: &str,
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
        let epoch_id = if let Some(row) = sqlx::query(
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
            *epoch.epoch_id().as_uuid()
        } else if let Some(existing) = sqlx::query(
            "SELECT epoch_id FROM mcp_catalog_epochs WHERE session_id = $1 AND server_alias = $2 AND canonical_digest = $3",
        )
        .bind(*epoch.session_id().as_uuid())
        .bind(binding_kind)
        .bind(epoch.digest())
        .fetch_optional(&mut *tx)
        .await?
        {
            existing.get("epoch_id")
        } else {
            sqlx::query("INSERT INTO mcp_catalog_epochs (epoch_id, session_id, server_alias, refresh_identity, canonical_digest, connection_binding, descriptor_snapshot) VALUES ($1, $2, $3, $4, $5, $6, $7)")
            .bind(*epoch.epoch_id().as_uuid()).bind(*epoch.session_id().as_uuid()).bind(binding_kind).bind(epoch.epoch_id().as_uuid().to_string()).bind(epoch.digest()).bind(json!({"kind":binding_kind})).bind(snapshot)
            .execute(&mut *tx).await?;
            *epoch.epoch_id().as_uuid()
        };
        sqlx::query("UPDATE sessions SET current_catalog_epoch = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2").bind(epoch_id).bind(*epoch.session_id().as_uuid()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn current_catalog(
        &self,
        session_id: SessionId,
    ) -> Result<ToolCatalogSnapshot, StoreError> {
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
            .ok_or_else(|| {
                StoreError::InvalidData("catalog snapshot tools are malformed".to_owned())
            })?
            .iter()
            .map(|entry| {
                let descriptor = entry.get("descriptor").ok_or_else(|| {
                    StoreError::InvalidData("catalog entry descriptor is malformed".to_owned())
                })?;
                let tool = CatalogTool::from_normalized(descriptor)
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                if entry.get("qualified_name").and_then(Value::as_str)
                    != Some(tool.qualified_name())
                    || entry.get("descriptor_digest").and_then(Value::as_str)
                        != Some(tool.descriptor_digest())
                {
                    return Err(StoreError::CatalogScopeViolation);
                }
                Ok(tool)
            })
            .collect::<Result<Vec<_>, _>>()?;
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

    pub async fn submit_user_task(
        &self,
        principal: &PrincipalId,
        request: SubmitUserTask,
    ) -> Result<AcceptedUserTask, StoreError> {
        request
            .validate()
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let goal_digest = request
            .goal
            .digest()
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let target = serde_json::to_value(&request.target)
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let preferences = serde_json::to_value(&request.preferences)
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let success_criteria = serde_json::to_value(&request.goal.success)
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let constraints = serde_json::to_value(&request.goal.constraints)
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let blocked_when = serde_json::to_value(&request.goal.blocked_when)
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let mut tx = self.pool.begin().await?;

        let session: Option<(String, String)> =
            sqlx::query_as("SELECT principal, state FROM sessions WHERE id = $1 FOR UPDATE")
                .bind(request.session_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?;
        let Some((session_principal, state)) = session else {
            return Err(StoreError::SessionNotFound);
        };
        if session_principal != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if state != "active" {
            return Err(StoreError::SessionClosed);
        }

        let replay: Option<(uuid::Uuid, String)> = sqlx::query_as(
            "SELECT id, request_digest FROM user_tasks \
             WHERE session_id = $1 AND submission_idempotency_key = $2",
        )
        .bind(request.session_id.as_uuid())
        .bind(request.idempotency_key.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((task_id, stored_digest)) = replay {
            let accepted =
                get_user_task_in_tx(&mut tx, request.session_id.clone(), task_id).await?;
            let mut replay_request = request;
            if replay_request.agent.version.is_none() {
                replay_request.agent.version = Some(accepted.definition.version);
            }
            let replay_digest = replay_request
                .request_digest()
                .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
            if stored_digest != replay_digest {
                return Err(StoreError::IdempotencyConflict);
            }
            tx.commit().await?;
            return Ok(accepted);
        }

        let definition =
            resolve_agent_in_tx(&mut tx, &request.agent.name, request.agent.version).await?;
        let mut accepted_request = request.clone();
        accepted_request.agent.version = Some(definition.version);
        let request_digest = accepted_request
            .request_digest()
            .map_err(|error| StoreError::InvalidUserTask(error.to_string()))?;
        let task_id = UserTaskId::new(uuid::Uuid::now_v7());
        sqlx::query(
            "INSERT INTO user_tasks \
             (id, session_id, principal, definition_id, submission_idempotency_key, \
              request_digest, target, preferences) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(task_id.as_uuid())
        .bind(request.session_id.as_uuid())
        .bind(principal.as_str())
        .bind(definition.id)
        .bind(request.idempotency_key.as_str())
        .bind(&request_digest)
        .bind(target)
        .bind(preferences)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO goal_contracts \
             (id, task_id, contract_version, objective, success_criteria, constraints, \
              blocked_when, contract_digest) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(task_id.as_uuid())
        .bind(i32::from(request.goal.version))
        .bind(&request.goal.objective)
        .bind(success_criteria)
        .bind(constraints)
        .bind(blocked_when)
        .bind(&goal_digest)
        .execute(&mut *tx)
        .await?;

        let accepted = get_user_task_in_tx(&mut tx, request.session_id, *task_id.as_uuid()).await?;
        tx.commit().await?;
        Ok(accepted)
    }

    pub async fn get_user_task(
        &self,
        session_id: SessionId,
        task_id: UserTaskId,
    ) -> Result<AcceptedUserTask, StoreError> {
        let mut tx = self.pool.begin().await?;
        let accepted = get_user_task_in_tx(&mut tx, session_id, *task_id.as_uuid()).await?;
        tx.commit().await?;
        Ok(accepted)
    }

    pub async fn start_run(&self, request: StartRun) -> Result<RunView, StoreError> {
        let input_digest = canonical_sha256(&request.input)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let bindings = request.input.as_object().ok_or_else(|| {
            StoreError::InvalidData("workflow input must be a JSON object".to_owned())
        })?;
        let mut tx = self.pool.begin().await?;
        let session = sqlx::query(
            "SELECT principal, state, current_catalog_epoch FROM sessions WHERE id = $1 FOR UPDATE",
        )
        .bind(*request.session_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::SessionNotFound)?;
        if session.get::<String, _>("state") != "active" {
            return Err(StoreError::SessionClosed);
        }
        let published_definition = resolve_definition_for_start(&mut tx, &request).await?;
        if published_definition.kind == DefinitionKind::Agent {
            return Err(StoreError::AgentRuntimeUnavailable);
        }
        let catalog_epoch = session
            .get::<Option<uuid::Uuid>, _>("current_catalog_epoch")
            .ok_or(StoreError::CatalogScopeViolation)?;
        let definition_id = published_definition.id;
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
                definition: published_definition,
                root_frame_id: crate::FrameId::new(root.get("id")),
                state: run_state(&existing.get::<String, _>("state"))?,
                revision: existing.get::<i64, _>("revision") as u64,
            });
        }
        let active_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE session_id = $1 AND state IN ('runnable', 'running', 'waiting_for_answer', 'waiting_for_permission', 'outcome_unknown')",
        )
        .bind(*request.session_id.as_uuid())
        .fetch_one(&mut *tx)
        .await?;
        if active_runs
            >= self
                .limits
                .max_active_runs_per_session
                .min(i64::MAX as usize) as i64
        {
            return Err(StoreError::QuotaExceeded("active_runs_per_session"));
        }
        let definition = sqlx::query(
            "SELECT tool_bindings, compiled_definition FROM workflow_definitions WHERE id = $1",
        )
        .bind(definition_id)
        .fetch_one(&mut *tx)
        .await?;
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
            .ok_or_else(|| {
                StoreError::InvalidData("catalog snapshot tools are malformed".to_owned())
            })?;
        let definition_bindings = definition.get::<Value, _>("tool_bindings");
        let definition_bindings = definition_bindings.as_array().ok_or_else(|| {
            StoreError::InvalidData("workflow tool bindings are malformed".to_owned())
        })?;
        let run_id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO runs (id, session_id, definition_id, start_idempotency_key, input, input_digest, state) VALUES ($1, $2, $3, $4, $5, $6, 'runnable')")
            .bind(run_id).bind(*request.session_id.as_uuid()).bind(definition_id).bind(request.idempotency_key.as_str()).bind(&request.input).bind(&input_digest).execute(&mut *tx).await?;
        for binding in definition_bindings {
            let qualified_name = binding
                .get("qualified_name")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    StoreError::InvalidData("workflow binding name is malformed".to_owned())
                })?;
            let descriptor_digest = binding
                .get("descriptor_digest")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    StoreError::InvalidData("workflow binding digest is malformed".to_owned())
                })?;
            let binding_epoch = binding
                .get("catalog_epoch_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    StoreError::InvalidData("workflow binding epoch is malformed".to_owned())
                })?;
            if binding_epoch != catalog_epoch.to_string() {
                return Err(StoreError::CatalogScopeViolation);
            }
            let tool = catalog_tools
                .iter()
                .find_map(|entry| {
                    let descriptor = entry.get("descriptor")?;
                    (entry.get("qualified_name")?.as_str() == Some(qualified_name)
                        && entry.get("descriptor_digest")?.as_str() == Some(descriptor_digest))
                    .then_some(descriptor)
                })
                .ok_or(StoreError::CatalogScopeViolation)?;
            sqlx::query("INSERT INTO run_tool_bindings (run_id, qualified_name, server_alias, catalog_epoch_id, descriptor_digest, capabilities, recovery_profile, descriptor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
                .bind(run_id).bind(qualified_name).bind(tool.get("server_alias").and_then(Value::as_str).ok_or_else(|| StoreError::InvalidData("catalog tool server alias is malformed".to_owned()))?).bind(catalog_epoch).bind(descriptor_digest).bind(tool.get("capabilities").cloned().ok_or_else(|| StoreError::InvalidData("catalog tool capabilities are malformed".to_owned()))?).bind(tool.get("recovery").and_then(Value::as_str).ok_or_else(|| StoreError::InvalidData("catalog tool recovery profile is malformed".to_owned()))?).bind(tool.clone()).execute(&mut *tx).await?;
        }
        let frame_id = uuid::Uuid::now_v7();
        let compiled_definition = definition.get::<Value, _>("compiled_definition");
        let task_name = compiled_definition
            .get("entry")
            .and_then(|entry| entry.get("task"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                StoreError::InvalidData("compiled workflow entry is malformed".to_owned())
            })?;
        sqlx::query("INSERT INTO frames (id, run_id, task_name, bindings, state) VALUES ($1, $2, $3, $4, 'runnable')")
            .bind(frame_id).bind(run_id).bind(task_name).bind(Value::Object(bindings.clone())).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO events (run_id, sequence, kind, metadata) VALUES ($1, 1, 'run.started', $2)")
            .bind(run_id).bind(json!({"definition_id": definition_id, "catalog_epoch_id": catalog_epoch})).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(RunView {
            id: crate::RunId::new(run_id),
            session_id: request.session_id,
            definition: published_definition,
            root_frame_id: crate::FrameId::new(frame_id),
            state: RunState::Runnable,
            revision: 0,
        })
    }

    pub async fn runnable_frames(
        &self,
        limit: i64,
    ) -> Result<Vec<super::RunnableFrame>, StoreError> {
        self.runnable_frames_query(
            "f.state = 'runnable' AND r.state IN ('runnable', 'running')",
            None,
            limit,
        )
        .await
    }

    pub async fn get_run(
        &self,
        session_id: crate::SessionId,
        run_id: crate::RunId,
    ) -> Result<super::RunView, StoreError> {
        let row = sqlx::query("SELECT r.id, r.session_id, r.state, r.revision, d.id AS definition_id, d.definition_kind, d.name AS definition_name, d.version AS definition_version, d.definition_hash FROM runs r JOIN workflow_definitions d ON d.id = r.definition_id WHERE r.id = $1 AND r.session_id = $2")
            .bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        let root =
            sqlx::query("SELECT id FROM frames WHERE run_id = $1 AND parent_frame_id IS NULL")
                .bind(*run_id.as_uuid())
                .fetch_one(&self.pool)
                .await?;
        Ok(super::RunView {
            id: crate::RunId::new(row.get("id")),
            session_id: crate::SessionId::new(row.get("session_id")),
            root_frame_id: crate::FrameId::new(root.get("id")),
            definition: PublishedDefinition {
                id: row.get("definition_id"),
                kind: definition_kind(&row.get::<String, _>("definition_kind"))?,
                name: row.get("definition_name"),
                version: row.get::<i32, _>("definition_version") as u32,
                definition_hash: row.get("definition_hash"),
            },
            state: run_state(&row.get::<String, _>("state"))?,
            revision: row.get::<i64, _>("revision") as u64,
        })
    }

    pub async fn runnable_frames_for_run(
        &self,
        run_id: crate::RunId,
        limit: i64,
    ) -> Result<Vec<super::RunnableFrame>, StoreError> {
        self.runnable_frames_query(
            "f.state = 'runnable' AND r.state IN ('runnable', 'running') AND f.run_id = $1",
            Some(run_id),
            limit,
        )
        .await
    }

    async fn runnable_frames_query(
        &self,
        predicate: &str,
        run_id: Option<crate::RunId>,
        limit: i64,
    ) -> Result<Vec<super::RunnableFrame>, StoreError> {
        let sql = format!(
            "SELECT f.id, f.run_id, r.session_id, f.task_name, f.outcome_name, f.statement_index, f.bindings, f.revision, d.compiled_definition FROM frames f JOIN runs r ON r.id = f.run_id JOIN workflow_definitions d ON d.id = r.definition_id WHERE {predicate} ORDER BY f.created_at, f.id LIMIT {}",
            if run_id.is_some() { "$2" } else { "$1" }
        );
        let mut query = sqlx::query(&sql);
        if let Some(run_id) = run_id {
            query = query.bind(*run_id.as_uuid());
        }
        let rows = query
            .bind(limit.clamp(1, 256))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(super::RunnableFrame {
                    id: crate::FrameId::new(row.get("id")),
                    run_id: crate::RunId::new(row.get("run_id")),
                    session_id: crate::SessionId::new(row.get("session_id")),
                    task_name: row.get("task_name"),
                    outcome_name: row.get("outcome_name"),
                    statement_index: row.get::<i32, _>("statement_index") as usize,
                    bindings: row.get("bindings"),
                    revision: row.get::<i64, _>("revision") as u64,
                    workflow: serde_json::from_value(row.get("compiled_definition"))
                        .map_err(|error| StoreError::InvalidData(error.to_string()))?,
                })
            })
            .collect()
    }

    pub async fn run_tool_binding(
        &self,
        run_id: crate::RunId,
        qualified_name: &str,
    ) -> Result<super::RunToolBinding, StoreError> {
        let row = sqlx::query("SELECT qualified_name, descriptor_digest, capabilities, descriptor FROM run_tool_bindings WHERE run_id = $1 AND qualified_name = $2")
            .bind(*run_id.as_uuid()).bind(qualified_name).fetch_optional(&self.pool).await?
            .ok_or(StoreError::CatalogScopeViolation)?;
        let capabilities = serde_json::from_value(row.get("capabilities"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        Ok(super::RunToolBinding {
            qualified_name: row.get("qualified_name"),
            descriptor_digest: row.get("descriptor_digest"),
            capabilities,
            descriptor: row.get::<Option<Value>, _>("descriptor").ok_or_else(|| {
                StoreError::InvalidData("run tool descriptor is absent".to_owned())
            })?,
        })
    }

    pub async fn run_tool_bindings(
        &self,
        run_id: crate::RunId,
    ) -> Result<Vec<super::RunToolBinding>, StoreError> {
        let rows = sqlx::query("SELECT qualified_name, descriptor_digest, capabilities, descriptor FROM run_tool_bindings WHERE run_id = $1 ORDER BY qualified_name")
            .bind(*run_id.as_uuid()).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let capabilities = serde_json::from_value(row.get("capabilities"))
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let descriptor = row.get::<Option<Value>, _>("descriptor").ok_or_else(|| {
                    StoreError::InvalidData("run tool descriptor is absent".to_owned())
                })?;
                Ok(super::RunToolBinding {
                    qualified_name: row.get("qualified_name"),
                    descriptor_digest: row.get("descriptor_digest"),
                    capabilities,
                    descriptor,
                })
            })
            .collect()
    }

    pub async fn policy_revision(
        &self,
        session_id: crate::SessionId,
    ) -> Result<crate::PolicyRevision, StoreError> {
        let row = sqlx::query("SELECT current_policy_revision FROM sessions WHERE id = $1")
            .bind(*session_id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(crate::PolicyRevision::new(
            row.get::<i64, _>("current_policy_revision") as u64,
        ))
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
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT attempt) AS count FROM model_items WHERE frame_id = $1",
        )
        .bind(*frame_id.as_uuid())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("count") as u64)
    }

    pub async fn permission_suspension(
        &self,
        session_id: crate::SessionId,
        run_id: crate::RunId,
        request_id: crate::PermissionRequestId,
    ) -> Result<super::SuspensionFrame, StoreError> {
        let row = sqlx::query("SELECT f.id, f.revision FROM permission_requests p JOIN frames f ON f.id = p.frame_id JOIN runs r ON r.id = p.run_id WHERE p.id = $1 AND p.run_id = $2 AND r.session_id = $3 AND p.state IN ('pending', 'resolved')")
            .bind(*request_id.as_uuid()).bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(super::SuspensionFrame {
            frame_id: crate::FrameId::new(row.get("id")),
            revision: row.get::<i64, _>("revision") as u64,
        })
    }

    pub async fn pending_permission(
        &self,
        session_id: crate::SessionId,
        run_id: crate::RunId,
        request_id: crate::PermissionRequestId,
    ) -> Result<super::PendingPermission, StoreError> {
        let row = sqlx::query("SELECT f.id, f.revision, t.id AS tool_call_id, p.provider_call_id, p.qualified_tool, p.descriptor_digest, t.arguments, p.state, p.resolution_idempotency_key FROM permission_requests p JOIN frames f ON f.id = p.frame_id JOIN runs r ON r.id = p.run_id JOIN tool_calls t ON t.frame_id = p.frame_id AND t.provider_call_id = p.provider_call_id WHERE p.id = $1 AND p.run_id = $2 AND r.session_id = $3 AND p.state IN ('pending', 'resolved')")
            .bind(*request_id.as_uuid()).bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(super::PendingPermission {
            tool_call_id: row.get("tool_call_id"),
            frame_id: crate::FrameId::new(row.get("id")),
            revision: row.get::<i64, _>("revision") as u64,
            provider_call_id: row.get("provider_call_id"),
            qualified_name: row.get("qualified_tool"),
            descriptor_digest: row.get("descriptor_digest"),
            arguments: row.get("arguments"),
            resolved: row.get::<String, _>("state") == "resolved",
            resolution_idempotency_key: row.get("resolution_idempotency_key"),
        })
    }

    pub async fn question_suspension(
        &self,
        session_id: crate::SessionId,
        run_id: crate::RunId,
        question_id: uuid::Uuid,
    ) -> Result<super::SuspensionFrame, StoreError> {
        let row = sqlx::query("SELECT f.id, f.revision FROM questions q JOIN frames f ON f.id = q.frame_id JOIN runs r ON r.id = q.run_id WHERE q.id = $1 AND q.run_id = $2 AND r.session_id = $3 AND q.state IN ('pending', 'answered')")
            .bind(question_id).bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).fetch_optional(&self.pool).await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(super::SuspensionFrame {
            frame_id: crate::FrameId::new(row.get("id")),
            revision: row.get::<i64, _>("revision") as u64,
        })
    }

    pub async fn run_revision(
        &self,
        session_id: crate::SessionId,
        run_id: crate::RunId,
    ) -> Result<u64, StoreError> {
        let row = sqlx::query("SELECT revision FROM runs WHERE id = $1 AND session_id = $2")
            .bind(*run_id.as_uuid())
            .bind(*session_id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        Ok(row.get::<i64, _>("revision") as u64)
    }

    pub async fn events_after(
        &self,
        session_id: crate::SessionId,
        run_id: crate::RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<super::RunEvent>, StoreError> {
        let rows = sqlx::query("SELECT e.sequence, e.kind, e.metadata, e.created_at FROM events e JOIN runs r ON r.id = e.run_id WHERE e.run_id = $1 AND r.session_id = $2 AND e.sequence > $3 ORDER BY e.sequence LIMIT $4")
            .bind(*run_id.as_uuid()).bind(*session_id.as_uuid()).bind(after as i64).bind(i64::from(limit.clamp(1, 256))).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|row| super::RunEvent {
                sequence: row.get::<i64, _>("sequence") as u64,
                kind: row.get("kind"),
                metadata: row.get("metadata"),
                created_at: row.get("created_at"),
            })
            .collect())
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
            .bind(limit)
            .fetch_all(&mut *tx)
            .await?;
        for frame in &frames {
            let frame_id: uuid::Uuid = frame.get("id");
            let run_id: uuid::Uuid = frame.get("run_id");
            sqlx::query("UPDATE frames SET state = 'runnable', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
                .bind(frame_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE runs SET state = 'runnable', updated_at = CURRENT_TIMESTAMP WHERE id = $1 AND state = 'running'")
                .bind(run_id).execute(&mut *tx).await?;
            append_event(
                &mut tx,
                run_id,
                "model.requeued",
                json!({"frame_id": frame_id}),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(frames.len() as u64)
    }

    pub async fn recoverable_tool_calls(
        &self,
        limit: i64,
    ) -> Result<Vec<super::RecoverableToolCall>, StoreError> {
        self.recoverable_tool_calls_excluding(limit, &std::collections::BTreeSet::new())
            .await
    }

    pub async fn recoverable_tool_call(
        &self,
        session_id: crate::SessionId,
        run_id: crate::RunId,
        tool_call_id: uuid::Uuid,
    ) -> Result<super::RecoverableToolCall, StoreError> {
        let row = sqlx::query("SELECT t.id, t.frame_id, r.id AS run_id, r.session_id, f.revision, t.qualified_name, t.descriptor_digest, t.idempotency_key, t.arguments, b.recovery_profile FROM tool_calls t JOIN frames f ON f.id = t.frame_id JOIN runs r ON r.id = f.run_id JOIN run_tool_bindings b ON b.run_id = r.id AND b.qualified_name = t.qualified_name AND b.descriptor_digest = t.descriptor_digest WHERE t.id = $1 AND r.id = $2 AND r.session_id = $3 AND t.state = 'intent' AND f.state = 'running' AND r.state = 'running'")
            .bind(tool_call_id)
            .bind(*run_id.as_uuid())
            .bind(*session_id.as_uuid())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::FrameNotFound)?;
        Ok(super::RecoverableToolCall {
            id: row.get("id"),
            frame_id: crate::FrameId::new(row.get("frame_id")),
            run_id: crate::RunId::new(row.get("run_id")),
            session_id: crate::SessionId::new(row.get("session_id")),
            revision: row.get::<i64, _>("revision") as u64,
            qualified_name: row.get("qualified_name"),
            descriptor_digest: row.get("descriptor_digest"),
            idempotency_key: crate::IdempotencyKey::new(row.get::<String, _>("idempotency_key"))
                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
            arguments: row.get("arguments"),
            recovery: serde_json::from_value(Value::String(row.get("recovery_profile")))
                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        })
    }

    pub async fn recoverable_tool_calls_excluding(
        &self,
        limit: i64,
        excluded_sessions: &std::collections::BTreeSet<crate::SessionId>,
    ) -> Result<Vec<super::RecoverableToolCall>, StoreError> {
        let excluded_sessions = excluded_sessions
            .iter()
            .map(|session| *session.as_uuid())
            .collect::<Vec<_>>();
        let rows = sqlx::query("SELECT t.id, t.frame_id, r.id AS run_id, r.session_id, f.revision, t.qualified_name, t.descriptor_digest, t.idempotency_key, t.arguments, b.recovery_profile FROM tool_calls t JOIN frames f ON f.id = t.frame_id JOIN runs r ON r.id = f.run_id JOIN run_tool_bindings b ON b.run_id = r.id AND b.qualified_name = t.qualified_name AND b.descriptor_digest = t.descriptor_digest WHERE t.state = 'intent' AND f.state = 'running' AND r.state = 'running' AND NOT (r.session_id = ANY($2)) ORDER BY t.updated_at, t.id LIMIT $1")
            .bind(limit.clamp(1, 256))
            .bind(excluded_sessions)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(super::RecoverableToolCall {
                    id: row.get("id"),
                    frame_id: crate::FrameId::new(row.get("frame_id")),
                    run_id: crate::RunId::new(row.get("run_id")),
                    session_id: crate::SessionId::new(row.get("session_id")),
                    revision: row.get::<i64, _>("revision") as u64,
                    qualified_name: row.get("qualified_name"),
                    descriptor_digest: row.get("descriptor_digest"),
                    idempotency_key: crate::IdempotencyKey::new(
                        row.get::<String, _>("idempotency_key"),
                    )
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?,
                    arguments: row.get("arguments"),
                    recovery: serde_json::from_value(Value::String(row.get("recovery_profile")))
                        .map_err(|error| StoreError::InvalidData(error.to_string()))?,
                })
            })
            .collect()
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
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
        let call = sqlx::query(
            "SELECT state, recovery_attempts FROM tool_calls WHERE id = $1 AND frame_id = $2 FOR UPDATE",
        )
                .bind(tool_call_id)
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| {
                    StoreError::InvalidData("tool call was not found for frame".to_owned())
                })?;
        if call.get::<String, _>("state") != "intent" {
            return Err(StoreError::RevisionConflict);
        }
        if call.get::<i32, _>("recovery_attempts") as u32 >= retry_budget {
            tx.commit().await?;
            return Ok(None);
        }
        let revision = expected_revision + 1;
        sqlx::query("UPDATE tool_calls SET recovery_attempts = recovery_attempts + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(tool_call_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
        )
        .bind(revision as i64)
        .bind(*frame_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            run_id,
            "tool.status_check_intent",
            json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id}),
        )
        .await?;
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
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
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
        sqlx::query(
            "UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
        )
        .bind(revision as i64)
        .bind(*frame_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            run_id,
            "tool.recovery_retry",
            json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id}),
        )
        .await?;
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
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
        let call =
            sqlx::query("SELECT state FROM tool_calls WHERE id = $1 AND frame_id = $2 FOR UPDATE")
                .bind(tool_call_id)
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| {
                    StoreError::InvalidData("tool call was not found for frame".to_owned())
                })?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
        if call.get::<String, _>("state") != "intent" {
            return Err(StoreError::RevisionConflict);
        }
        sqlx::query("UPDATE tool_calls SET state = 'outcome_unknown', updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(tool_call_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE frames SET state = 'outcome_unknown', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'outcome_unknown', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(run_id).execute(&mut *tx).await?;
        append_event(
            &mut tx,
            run_id,
            "tool.outcome_unknown",
            json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id}),
        )
        .await?;
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
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
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
        let attempt = sqlx::query(
            "SELECT COALESCE(MAX(attempt), 0) + 1 AS attempt FROM model_items WHERE frame_id = $1",
        )
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
        append_event(
            &mut tx,
            run_id,
            "model.intent",
            json!({"frame_id": frame_id.as_uuid(), "attempt": attempt}),
        )
        .await?;
        tx.commit().await?;
        Ok(super::ModelAttempt {
            frame_id,
            attempt,
            revision,
        })
    }

    /// Starts a durable run-wide active-I/O interval and returns its remaining budget in ms.
    pub async fn begin_active_execution(
        &self,
        run_id: crate::RunId,
        limit_millis: u64,
    ) -> Result<Option<u64>, StoreError> {
        let mut tx = self.pool.begin().await?;
        lock_run(&mut tx, *run_id.as_uuid()).await?;
        let run = sqlx::query("SELECT state, active_execution_millis, active_execution_count FROM runs WHERE id = $1 FOR UPDATE")
            .bind(*run_id.as_uuid())
            .fetch_one(&mut *tx)
            .await?;
        if !matches!(
            run.get::<String, _>("state").as_str(),
            "runnable" | "running" | "waiting_for_answer" | "waiting_for_permission"
        ) {
            return Err(StoreError::RevisionConflict);
        }
        let consumed = run.get::<i64, _>("active_execution_millis") as u64;
        if consumed >= limit_millis {
            tx.commit().await?;
            return Ok(None);
        }
        sqlx::query("UPDATE runs SET active_execution_count = active_execution_count + 1, active_execution_started_at = COALESCE(active_execution_started_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP WHERE id = $1")
            .bind(*run_id.as_uuid())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(limit_millis - consumed))
    }

    /// Ends one active-I/O interval and returns whether the durable budget is exhausted.
    pub async fn finish_active_execution(
        &self,
        run_id: crate::RunId,
        limit_millis: u64,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        lock_run(&mut tx, *run_id.as_uuid()).await?;
        let run = sqlx::query("SELECT active_execution_millis, active_execution_started_at, active_execution_count FROM runs WHERE id = $1 FOR UPDATE")
            .bind(*run_id.as_uuid())
            .fetch_one(&mut *tx)
            .await?;
        let count = run.get::<i32, _>("active_execution_count");
        if count <= 0 {
            return Err(StoreError::InvalidData(
                "active execution lease was not held".to_owned(),
            ));
        }
        if count > 1 {
            sqlx::query("UPDATE runs SET active_execution_count = active_execution_count - 1, updated_at = CURRENT_TIMESTAMP WHERE id = $1")
                .bind(*run_id.as_uuid())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(false);
        }
        let started = run
            .get::<Option<time::OffsetDateTime>, _>("active_execution_started_at")
            .ok_or_else(|| {
                StoreError::InvalidData("active execution start is absent".to_owned())
            })?;
        let elapsed = (time::OffsetDateTime::now_utc() - started)
            .whole_milliseconds()
            .max(0) as u64;
        let consumed = run.get::<i64, _>("active_execution_millis") as u64;
        let total = consumed.saturating_add(elapsed);
        sqlx::query("UPDATE runs SET active_execution_millis = $1, active_execution_count = 0, active_execution_started_at = NULL, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(total.min(i64::MAX as u64) as i64)
            .bind(*run_id.as_uuid())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(total >= limit_millis)
    }

    pub async fn fail_frame_with_code(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        code: &str,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame = sqlx::query("SELECT revision, state FROM frames WHERE id = $1 FOR UPDATE")
            .bind(*frame_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || !matches!(
                frame.get::<String, _>("state").as_str(),
                "runnable" | "running"
            )
        {
            return Err(StoreError::RevisionConflict);
        }
        let failure = json!({"code":code});
        sqlx::query("UPDATE frames SET state = 'failed', failure = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
            .bind(&failure)
            .bind((expected_revision + 1) as i64)
            .bind(*frame_id.as_uuid())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE runs SET state = 'failed', failure = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
            .bind(&failure)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        append_event(
            &mut tx,
            run_id,
            "run.failed",
            json!({"code":code, "frame_id":frame_id.as_uuid()}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn commit_model_attempt(
        &self,
        frame_id: crate::FrameId,
        attempt: u32,
        expected_revision: u64,
        items: Vec<Value>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let run_id = lock_run_for_frame(&mut tx, *frame_id.as_uuid()).await?;
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
        let intent = sqlx::query("SELECT 1 FROM model_items WHERE frame_id = $1 AND attempt = $2 AND position = 0 AND direction = 'input'")
            .bind(*frame_id.as_uuid()).bind(attempt as i32).fetch_optional(&mut *tx).await?;
        if intent.is_none() {
            return Err(StoreError::InvalidData(
                "model result has no matching intent".to_owned(),
            ));
        }
        let state = frame.get::<String, _>("state");
        if frame.get::<i64, _>("revision") as u64 != expected_revision || state != "running" {
            if state == "cancelled" {
                let already_recorded = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM model_items WHERE frame_id = $1 AND attempt = $2 AND direction = 'output')",
                )
                .bind(*frame_id.as_uuid())
                .bind(attempt as i32)
                .fetch_one(&mut *tx)
                .await?;
                if !already_recorded {
                    for (position, item) in items.into_iter().enumerate() {
                        sqlx::query("INSERT INTO model_items (frame_id, attempt, position, direction, item) VALUES ($1, $2, $3, 'output', $4)")
                            .bind(*frame_id.as_uuid()).bind(attempt as i32).bind(position as i32).bind(item).execute(&mut *tx).await?;
                    }
                    append_event(
                        &mut tx,
                        run_id,
                        "model.late_result",
                        json!({"frame_id": frame_id.as_uuid(), "attempt": attempt}),
                    )
                    .await?;
                }
                tx.commit().await?;
                return Ok(false);
            }
            return Err(StoreError::RevisionConflict);
        }
        for (position, item) in items.into_iter().enumerate() {
            sqlx::query("INSERT INTO model_items (frame_id, attempt, position, direction, item) VALUES ($1, $2, $3, 'output', $4)")
                .bind(*frame_id.as_uuid()).bind(attempt as i32).bind(position as i32).bind(item).execute(&mut *tx).await?;
        }
        let revision = expected_revision + 1;
        sqlx::query(
            "UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
        )
        .bind(revision as i64)
        .bind(*frame_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            run_id,
            "model.committed",
            json!({"frame_id": frame_id.as_uuid(), "attempt": attempt}),
        )
        .await?;
        tx.commit().await?;
        Ok(true)
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
        let frame = sqlx::query(
            "SELECT run_id, revision, state, bindings FROM frames WHERE id = $1 FOR UPDATE",
        )
        .bind(*frame_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || !matches!(
                frame.get::<String, _>("state").as_str(),
                "running" | "runnable"
            )
        {
            return Err(StoreError::RevisionConflict);
        }
        let mut bindings = frame
            .get::<Value, _>("bindings")
            .as_object()
            .cloned()
            .ok_or_else(|| StoreError::InvalidData("frame bindings are malformed".to_owned()))?;
        let fields = fields.as_object().ok_or_else(|| {
            StoreError::InvalidData("completion fields must be an object".to_owned())
        })?;
        bindings.extend(fields.clone());
        sqlx::query("UPDATE frames SET state = 'runnable', outcome_name = $1, statement_index = 0, bindings = $2, revision = $3, updated_at = CURRENT_TIMESTAMP WHERE id = $4")
            .bind(&outcome_name).bind(Value::Object(bindings)).bind((expected_revision + 1) as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        append_event(
            &mut tx,
            run_id,
            "model.completed_task",
            json!({"frame_id": frame_id.as_uuid(), "outcome": outcome_name}),
        )
        .await?;
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
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
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
        sqlx::query(
            "UPDATE frames SET revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
        )
        .bind(revision as i64)
        .bind(*frame_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        append_event(&mut tx, run_id, "tool.intent", json!({"frame_id": frame_id.as_uuid(), "tool_call_id": id, "qualified_name": qualified_name, "descriptor_digest": descriptor_digest})).await?;
        tx.commit().await?;
        Ok(super::ToolCallIntent {
            id,
            frame_id,
            provider_call_id,
            revision,
        })
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
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
        let call =
            sqlx::query("SELECT state FROM tool_calls WHERE id = $1 AND frame_id = $2 FOR UPDATE")
                .bind(tool_call_id)
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| {
                    StoreError::InvalidData("tool call was not found for frame".to_owned())
                })?;
        if frame.get::<String, _>("state") == "cancelled" {
            if call.get::<String, _>("state") == "intent" {
                match outcome {
                    Ok(result) => {
                        sqlx::query("UPDATE tool_calls SET state = 'completed', result = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                            .bind(result).bind(tool_call_id).execute(&mut *tx).await?;
                    }
                    Err(error) => {
                        sqlx::query("UPDATE tool_calls SET state = 'failed', error = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                            .bind(error).bind(tool_call_id).execute(&mut *tx).await?;
                    }
                }
                append_event(
                    &mut tx,
                    run_id,
                    "tool.late_result",
                    json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id}),
                )
                .await?;
            }
            tx.commit().await?;
            return Ok(());
        }
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "running"
        {
            return Err(StoreError::RevisionConflict);
        }
        if call.get::<String, _>("state") != "intent" {
            return Err(StoreError::RevisionConflict);
        }
        match outcome {
            Ok(result) => {
                sqlx::query("UPDATE tool_calls SET state = 'completed', result = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(result).bind(tool_call_id).execute(&mut *tx).await?;
                append_event(
                    &mut tx,
                    run_id,
                    "tool.completed",
                    json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id}),
                )
                .await?;
            }
            Err(error) => {
                sqlx::query("UPDATE tool_calls SET state = 'failed', error = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(error).bind(tool_call_id).execute(&mut *tx).await?;
                append_event(
                    &mut tx,
                    run_id,
                    "tool.failed",
                    json!({"frame_id": frame_id.as_uuid(), "tool_call_id": tool_call_id}),
                )
                .await?;
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
        let pending_permissions: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM permission_requests p JOIN runs r ON r.id = p.run_id WHERE r.session_id = $1 AND p.state = 'pending'",
        )
        .bind(session_id)
        .fetch_one(&mut *tx)
        .await?;
        if pending_permissions
            >= self
                .limits
                .max_open_permission_requests_per_session
                .min(i64::MAX as usize) as i64
        {
            return Err(StoreError::QuotaExceeded(
                "open_permission_requests_per_session",
            ));
        }
        lock_run(&mut tx, run_id).await?;
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
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
            || canonical_sha256(&call.get::<Value, _>("arguments"))
                .map_err(|error| StoreError::InvalidData(error.to_string()))?
                != request.input_digest
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
            if request.get::<Option<String>, _>("resolution_idempotency_key")
                == Some(idempotency_key)
            {
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
        sqlx::query(
            "UPDATE runs SET state = 'running', updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE tool_calls SET state = 'intent', updated_at = CURRENT_TIMESTAMP WHERE frame_id = $1 AND provider_call_id = (SELECT provider_call_id FROM permission_requests WHERE id = $2)")
            .bind(*frame_id.as_uuid()).bind(*request_id.as_uuid()).execute(&mut *tx).await?;
        append_event(
            &mut tx,
            run_id,
            "permission.resolved",
            json!({"frame_id": frame_id.as_uuid(), "request_id": request_id.as_uuid()}),
        )
        .await?;
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
        let frame =
            sqlx::query("SELECT run_id, revision, state FROM frames WHERE id = $1 FOR UPDATE")
                .bind(*frame_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || !matches!(
                frame.get::<String, _>("state").as_str(),
                "running" | "runnable"
            )
        {
            return Err(StoreError::RevisionConflict);
        }
        question
            .schema
            .validate_definition()
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let schema = serde_json::to_value(&question.schema)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
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
        let frame = sqlx::query(
            "SELECT run_id, revision, state, bindings FROM frames WHERE id = $1 FOR UPDATE",
        )
        .bind(*frame_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::FrameNotFound)?;
        let question = sqlx::query("SELECT binding, schema, state, answer_idempotency_key FROM questions WHERE id = $1 AND frame_id = $2 FOR UPDATE")
            .bind(question_id).bind(*frame_id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| StoreError::InvalidData("question was not found for frame".to_owned()))?;
        if question.get::<String, _>("state") == "answered" {
            if question.get::<Option<String>, _>("answer_idempotency_key") == Some(idempotency_key)
            {
                return Ok(());
            }
            return Err(StoreError::IdempotencyConflict);
        }
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || frame.get::<String, _>("state") != "waiting_for_answer"
            || question.get::<String, _>("state") != "pending"
        {
            return Err(StoreError::RevisionConflict);
        }
        let schema: crate::ClosedSchema = serde_json::from_value(question.get("schema"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        schema
            .validate(&answer)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let mut bindings = frame
            .get::<Value, _>("bindings")
            .as_object()
            .cloned()
            .ok_or_else(|| StoreError::InvalidData("frame bindings are malformed".to_owned()))?;
        bindings.insert(question.get("binding"), answer.clone());
        sqlx::query("UPDATE questions SET state = 'answered', answer = $1, answer_idempotency_key = $2, answered_at = CURRENT_TIMESTAMP WHERE id = $3")
            .bind(answer).bind(idempotency_key).bind(question_id).execute(&mut *tx).await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'runnable', suspension_id = NULL, bindings = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
            .bind(Value::Object(bindings)).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE runs SET state = 'runnable', updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        append_event(
            &mut tx,
            run_id,
            "question.answered",
            json!({"frame_id": frame_id.as_uuid(), "question_id": question_id}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn apply_frame_command(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        command: super::FrameCommand,
    ) -> Result<(), StoreError> {
        self.apply_frame_command_with_step_limit(frame_id, expected_revision, command, u64::MAX)
            .await
    }

    pub async fn apply_frame_command_with_step_limit(
        &self,
        frame_id: crate::FrameId,
        expected_revision: u64,
        command: super::FrameCommand,
        step_limit: u64,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let metadata = sqlx::query("SELECT run_id, parent_frame_id FROM frames WHERE id = $1")
            .bind(*frame_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
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
            .find(|frame| frame.get::<uuid::Uuid, _>("id") == *frame_id.as_uuid())
            .ok_or(StoreError::FrameNotFound)?;
        if frame.get::<i64, _>("revision") as u64 != expected_revision
            || !matches!(
                frame.get::<String, _>("state").as_str(),
                "running" | "runnable"
            )
        {
            return Err(StoreError::RevisionConflict);
        }
        let consumed = sqlx::query(
            "UPDATE runs SET step_count = step_count + 1, updated_at = CURRENT_TIMESTAMP \
             WHERE id = $1 AND step_count < $2 RETURNING step_count",
        )
        .bind(run_id)
        .bind(step_limit.min(i64::MAX as u64) as i64)
        .fetch_optional(&mut *tx)
        .await?;
        if consumed.is_none() {
            let failure = json!({"code":"step_limit_exceeded"});
            sqlx::query(
                "UPDATE frames SET state = 'failed', failure = $1, revision = $2, \
                 updated_at = CURRENT_TIMESTAMP WHERE id = $3",
            )
            .bind(&failure)
            .bind((expected_revision + 1) as i64)
            .bind(*frame_id.as_uuid())
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE runs SET state = 'failed', failure = $1, updated_at = CURRENT_TIMESTAMP \
                 WHERE id = $2",
            )
            .bind(&failure)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
            append_event(
                &mut tx,
                run_id,
                "run.failed",
                json!({"code":"step_limit_exceeded", "frame_id": frame_id.as_uuid()}),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
        let revision = expected_revision + 1;
        match command {
            super::FrameCommand::StartCall(child) => {
                insert_child_frame(&mut tx, run_id, *frame_id.as_uuid(), child).await?;
                sqlx::query("UPDATE frames SET state = 'waiting_for_children', statement_index = statement_index + 1, revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                append_event(
                    &mut tx,
                    run_id,
                    "frame.call_started",
                    json!({"frame_id": frame_id.as_uuid()}),
                )
                .await?;
            }
            super::FrameCommand::StartParallel(children) => {
                if children.is_empty() {
                    return Err(StoreError::InvalidData(
                        "parallel command has no children".to_owned(),
                    ));
                }
                for child in children {
                    insert_child_frame(&mut tx, run_id, *frame_id.as_uuid(), child).await?;
                }
                sqlx::query("UPDATE frames SET state = 'waiting_for_children', statement_index = statement_index + 1, revision = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                append_event(
                    &mut tx,
                    run_id,
                    "frame.parallel_started",
                    json!({"frame_id": frame_id.as_uuid()}),
                )
                .await?;
            }
            super::FrameCommand::SuspendAsk(question) => {
                question
                    .schema
                    .validate_definition()
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let schema = serde_json::to_value(&question.schema)
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                sqlx::query("INSERT INTO questions (id, run_id, frame_id, binding, text, schema, state) VALUES ($1, $2, $3, $4, $5, $6, 'pending')")
                    .bind(question.id).bind(run_id).bind(*frame_id.as_uuid()).bind(&question.binding).bind(&question.text).bind(schema).execute(&mut *tx).await?;
                sqlx::query("UPDATE frames SET state = 'waiting_for_answer', suspension_id = $1, statement_index = statement_index + 1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
                    .bind(question.id).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                sqlx::query("UPDATE runs SET state = 'waiting_for_answer', updated_at = CURRENT_TIMESTAMP WHERE id = $1").bind(run_id).execute(&mut *tx).await?;
                append_event(&mut tx, run_id, "question.requested", json!({"frame_id": frame_id.as_uuid(), "question_id": question.id, "binding": question.binding})).await?;
            }
            super::FrameCommand::Goto {
                task_name,
                bindings,
            } => {
                sqlx::query("UPDATE frames SET task_name = $1, bindings = $2, state = 'runnable', revision = $3, updated_at = CURRENT_TIMESTAMP WHERE id = $4")
                    .bind(task_name).bind(serde_json::to_value(bindings).map_err(|error| StoreError::InvalidData(error.to_string()))?).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                append_event(
                    &mut tx,
                    run_id,
                    "frame.goto",
                    json!({"frame_id": frame_id.as_uuid()}),
                )
                .await?;
            }
            super::FrameCommand::Complete(result) => {
                sqlx::query("UPDATE frames SET state = 'completed', result = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
                    .bind(result.clone()).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                if frame
                    .get::<Option<uuid::Uuid>, _>("parent_frame_id")
                    .is_none()
                {
                    sqlx::query("UPDATE runs SET state = 'completed', output = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                        .bind(result).bind(run_id).execute(&mut *tx).await?;
                } else {
                    let parent = locked_frames
                        .iter()
                        .find(|parent| Some(parent.get::<uuid::Uuid, _>("id")) == parent_id)
                        .ok_or(StoreError::FrameNotFound)?;
                    resume_parent_after_child(&mut tx, run_id, frame, parent, result).await?;
                }
                append_event(
                    &mut tx,
                    run_id,
                    "frame.completed",
                    json!({"frame_id": frame_id.as_uuid()}),
                )
                .await?;
            }
            super::FrameCommand::Fail { code, message } => {
                let failure = json!({"code": code, "message": message});
                sqlx::query("UPDATE frames SET state = 'failed', failure = $1, revision = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $3")
                    .bind(&failure).bind(revision as i64).bind(*frame_id.as_uuid()).execute(&mut *tx).await?;
                sqlx::query("UPDATE runs SET state = 'failed', failure = $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
                    .bind(failure).bind(run_id).execute(&mut *tx).await?;
                append_event(
                    &mut tx,
                    run_id,
                    "frame.failed",
                    json!({"frame_id": frame_id.as_uuid()}),
                )
                .await?;
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
            if run.get::<Option<String>, _>("cancellation_idempotency_key") == Some(idempotency_key)
            {
                return Ok(());
            }
            return Err(StoreError::IdempotencyConflict);
        }
        if run.get::<i64, _>("revision") as u64 != expected_revision
            || matches!(
                run.get::<String, _>("state").as_str(),
                "completed" | "failed"
            )
        {
            return Err(StoreError::RevisionConflict);
        }
        sqlx::query(
            "SELECT id FROM frames WHERE run_id = $1 AND state NOT IN ('completed', 'failed', 'cancelled') ORDER BY id FOR UPDATE",
        )
        .bind(*run_id.as_uuid())
        .fetch_all(&mut *tx)
        .await?;
        let revision = expected_revision + 1;
        sqlx::query("UPDATE frames SET state = 'cancelled', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE run_id = $1 AND state NOT IN ('completed', 'failed', 'cancelled')")
            .bind(*run_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET state = 'cancelled', revision = $1, cancellation_idempotency_key = $2, cancellation_reason = $3, updated_at = CURRENT_TIMESTAMP WHERE id = $4")
            .bind(revision as i64).bind(idempotency_key).bind(reason.clone().map(Value::String)).bind(*run_id.as_uuid()).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE questions SET state = 'cancelled' WHERE run_id = $1 AND state = 'pending'",
        )
        .bind(*run_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE permission_requests SET state = 'cancelled' WHERE run_id = $1 AND state = 'pending'")
            .bind(*run_id.as_uuid()).execute(&mut *tx).await?;
        append_event(
            &mut tx,
            *run_id.as_uuid(),
            "run.cancelled",
            json!({"reason": reason}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn publish(
        &self,
        session: SessionId,
        workflow: &CompiledDefinition,
        source: &str,
    ) -> Result<PublishedDefinition, StoreError> {
        self.verify_bindings(session, workflow).await?;
        let compiled =
            serde_json::to_value(workflow).map_err(|e| StoreError::InvalidData(e.to_string()))?;
        let bindings = bindings_json(workflow);
        let mut tx = self.pool.begin().await?;
        if let Some(row) = sqlx::query("SELECT id, definition_kind, name, version, definition_hash FROM workflow_definitions WHERE definition_hash = $1").bind(&workflow.definition_hash).fetch_optional(&mut *tx).await? {
            return definition_from_row(&row);
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&workflow.name)
            .execute(&mut *tx)
            .await?;
        if let Some(row) = sqlx::query("SELECT id, definition_kind, name, version, definition_hash FROM workflow_definitions WHERE definition_hash = $1").bind(&workflow.definition_hash).fetch_optional(&mut *tx).await? {
            return definition_from_row(&row);
        }
        let version: i32 = sqlx::query(
            "SELECT COALESCE(MAX(version), 0) + 1 AS version FROM workflow_definitions WHERE name = $1",
        )
        .bind(&workflow.name)
        .fetch_one(&mut *tx)
        .await?
        .get("version");
        let id = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO workflow_definitions (id, definition_kind, name, version, definition_hash, source, compiled_definition, tool_bindings) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)").bind(id).bind(definition_kind_name(workflow.definition_kind)).bind(&workflow.name).bind(version).bind(&workflow.definition_hash).bind(source).bind(compiled).bind(bindings).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(PublishedDefinition {
            id,
            kind: workflow.definition_kind,
            name: workflow.name.clone(),
            version: version as u32,
            definition_hash: workflow.definition_hash.clone(),
        })
    }

    async fn verify_bindings(
        &self,
        session: SessionId,
        workflow: &CompiledDefinition,
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

async fn resolve_agent_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    name: &str,
    version: Option<u32>,
) -> Result<PublishedDefinition, StoreError> {
    let row = if let Some(version) = version {
        sqlx::query(
            "SELECT id, definition_kind, name, version, definition_hash \
             FROM workflow_definitions \
             WHERE name = $1 AND version = $2 AND definition_kind = 'agent'",
        )
        .bind(name)
        .bind(version as i32)
        .fetch_optional(&mut **tx)
        .await?
    } else {
        sqlx::query(
            "SELECT id, definition_kind, name, version, definition_hash \
             FROM workflow_definitions \
             WHERE name = $1 AND definition_kind = 'agent' \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&mut **tx)
        .await?
    };
    if let Some(row) = row {
        return definition_from_row(&row);
    }

    let other_kind_exists = if let Some(version) = version {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM workflow_definitions WHERE name = $1 AND version = $2)",
        )
        .bind(name)
        .bind(version as i32)
        .fetch_one(&mut **tx)
        .await?
    } else {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM workflow_definitions WHERE name = $1)",
        )
        .bind(name)
        .fetch_one(&mut **tx)
        .await?
    };
    if other_kind_exists {
        Err(StoreError::AgentDefinitionRequired)
    } else {
        Err(StoreError::DefinitionNotFound)
    }
}

async fn get_user_task_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    session_id: SessionId,
    task_id: uuid::Uuid,
) -> Result<AcceptedUserTask, StoreError> {
    let row = sqlx::query(
        "SELECT t.id AS task_id, t.session_id, t.principal, t.request_digest, \
                t.target, t.preferences, \
                t.state AS task_state, t.revision, t.created_at, \
                d.id AS definition_id, d.definition_kind, d.name AS definition_name, \
                d.version AS definition_version, d.definition_hash, \
                g.task_id AS goal_task_id, g.contract_version, g.objective, \
                g.success_criteria, g.constraints, \
                g.blocked_when, g.contract_digest \
         FROM user_tasks t \
         LEFT JOIN goal_contracts g ON g.task_id = t.id \
         JOIN workflow_definitions d ON d.id = t.definition_id \
         WHERE t.session_id = $1 AND t.id = $2",
    )
    .bind(session_id.as_uuid())
    .bind(task_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::UserTaskNotFound)?;
    accepted_user_task_from_row(&row)
}

fn accepted_user_task_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<AcceptedUserTask, StoreError> {
    let stored_data = |message: &str| StoreError::InvalidData(message.to_owned());
    if row.get::<String, _>("task_state") != "accepted" {
        return Err(stored_data("user task state is malformed"));
    }
    let revision = u64::try_from(row.get::<i64, _>("revision"))
        .map_err(|_| stored_data("user task revision is malformed"))?;
    if revision != 0 {
        return Err(stored_data("user task revision is malformed"));
    }
    let definition = PublishedDefinition {
        id: row.get("definition_id"),
        kind: definition_kind(&row.get::<String, _>("definition_kind"))?,
        name: row.get("definition_name"),
        version: u32::try_from(row.get::<i32, _>("definition_version"))
            .map_err(|_| stored_data("definition version is malformed"))?,
        definition_hash: row.get("definition_hash"),
    };
    if definition.kind != DefinitionKind::Agent {
        return Err(stored_data(
            "user task definition is not an agent definition",
        ));
    }
    if row.get::<Option<uuid::Uuid>, _>("goal_task_id").is_none() {
        return Err(stored_data("user task goal contract is missing"));
    }
    let target: TaskTarget = serde_json::from_value(row.get("target"))
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    let preferences: TaskPreferences = serde_json::from_value(row.get("preferences"))
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    let version = u16::try_from(row.get::<i32, _>("contract_version"))
        .map_err(|_| stored_data("goal contract version is malformed"))?;
    let goal = GoalContract {
        version,
        objective: row.get("objective"),
        success: serde_json::from_value(row.get("success_criteria"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        constraints: serde_json::from_value(row.get("constraints"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        blocked_when: serde_json::from_value(row.get("blocked_when"))
            .map_err(|error| StoreError::InvalidData(error.to_string()))?,
    };
    goal.validate()
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    let goal_digest: String = row.get("contract_digest");
    let recomputed_digest = goal
        .digest()
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    if goal_digest != recomputed_digest {
        return Err(stored_data(
            "goal contract digest does not match its contents",
        ));
    }
    let principal = PrincipalId::new(row.get::<String, _>("principal"))
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;

    let validation_request = SubmitUserTask {
        session_id: SessionId::new(row.get("session_id")),
        agent: crate::AgentSelector {
            name: definition.name.clone(),
            version: Some(definition.version),
        },
        target: target.clone(),
        preferences: preferences.clone(),
        goal: goal.clone(),
        idempotency_key: crate::IdempotencyKey::new("stored-user-task")
            .expect("static idempotency key is valid"),
    };
    validation_request
        .validate()
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    let request_digest: String = row.get("request_digest");
    let recomputed_request_digest = validation_request
        .request_digest()
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
    if request_digest != recomputed_request_digest {
        return Err(stored_data(
            "user task request digest does not match its contents",
        ));
    }

    Ok(AcceptedUserTask {
        id: UserTaskId::new(row.get("task_id")),
        session_id: validation_request.session_id,
        principal,
        definition,
        target,
        preferences,
        goal,
        goal_digest,
        revision,
        created_at: row.get("created_at"),
    })
}

fn catalog_requires_mcp_attachment(connection_binding: Option<&Value>) -> bool {
    connection_binding
        .and_then(|binding| binding.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "mcp_catalog")
}

fn catalog_json(epoch: &ToolCatalogSnapshot) -> Value {
    json!({"tools": epoch.tools().values().map(|tool| json!({"qualified_name": tool.qualified_name(), "descriptor_digest": tool.descriptor_digest(), "descriptor": tool.normalized_descriptor()})).collect::<Vec<_>>()})
}
fn bindings_json(workflow: &CompiledDefinition) -> Value {
    json!(workflow.tool_bindings.iter().map(|binding| json!({"qualified_name": binding.qualified_name, "catalog_epoch_id": binding.catalog_epoch_id, "descriptor_digest": binding.descriptor_digest})).collect::<Vec<_>>())
}

async fn resolve_definition_for_start(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &StartRun,
) -> Result<PublishedDefinition, StoreError> {
    let row = if let Some(version) = request.version {
        sqlx::query(
            "SELECT id, definition_kind, name, version, definition_hash \
             FROM workflow_definitions WHERE name = $1 AND version = $2",
        )
        .bind(&request.workflow_name)
        .bind(version as i32)
        .fetch_optional(&mut **tx)
        .await?
    } else {
        sqlx::query(
            "SELECT id, definition_kind, name, version, definition_hash \
             FROM workflow_definitions WHERE name = $1 ORDER BY version DESC LIMIT 1",
        )
        .bind(&request.workflow_name)
        .fetch_optional(&mut **tx)
        .await?
    }
    .ok_or(StoreError::DefinitionNotFound)?;
    definition_from_row(&row)
}

fn definition_from_row(row: &sqlx::postgres::PgRow) -> Result<PublishedDefinition, StoreError> {
    Ok(PublishedDefinition {
        id: row.get("id"),
        kind: definition_kind(&row.get::<String, _>("definition_kind"))?,
        name: row.get("name"),
        version: u32::try_from(row.get::<i32, _>("version"))
            .map_err(|_| StoreError::InvalidData("definition version is malformed".to_owned()))?,
        definition_hash: row.get("definition_hash"),
    })
}

const fn definition_kind_name(kind: DefinitionKind) -> &'static str {
    match kind {
        DefinitionKind::Workflow => "workflow",
        DefinitionKind::Agent => "agent",
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
    let sequence = sqlx::query(
        "SELECT COALESCE(MAX(sequence), 0) + 1 AS sequence FROM events WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_one(&mut **tx)
    .await?
    .get::<i64, _>("sequence");
    sqlx::query("INSERT INTO events (run_id, sequence, kind, metadata) VALUES ($1, $2, $3, $4)")
        .bind(run_id)
        .bind(sequence)
        .bind(kind)
        .bind(metadata)
        .execute(&mut **tx)
        .await?;
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

async fn lock_principal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &str,
) -> Result<(), StoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(principal)
        .execute(&mut **tx)
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
    let parent_id = child
        .get::<Option<uuid::Uuid>, _>("parent_frame_id")
        .ok_or_else(|| StoreError::InvalidData("child frame has no parent".to_owned()))?;
    let binding = child
        .get::<Option<String>, _>("parent_binding")
        .ok_or_else(|| StoreError::InvalidData("child frame has no parent binding".to_owned()))?;
    let mut bindings = parent
        .get::<Value, _>("bindings")
        .as_object()
        .cloned()
        .ok_or_else(|| StoreError::InvalidData("parent bindings are malformed".to_owned()))?;
    bindings.insert(binding, result);
    sqlx::query("UPDATE frames SET bindings = $1, revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE id = $2")
        .bind(Value::Object(bindings)).bind(parent_id).execute(&mut **tx).await?;
    let incomplete = sqlx::query(
        "SELECT 1 FROM frames WHERE parent_frame_id = $1 AND state <> 'completed' LIMIT 1",
    )
    .bind(parent_id)
    .fetch_optional(&mut **tx)
    .await?;
    if incomplete.is_none() {
        sqlx::query(
            "UPDATE frames SET state = 'runnable', updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(parent_id)
        .execute(&mut **tx)
        .await?;
        append_event(
            tx,
            run_id,
            "frame.children_completed",
            json!({"parent_frame_id": parent_id}),
        )
        .await?;
    }
    Ok(())
}
