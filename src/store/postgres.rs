use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

use crate::dsl::CompiledWorkflow;
use crate::{
    CatalogEpochId, PolicyRevision, PrincipalId, SessionId, ToolCatalogSnapshot, canonical_sha256,
};

use super::{CreateSession, PolicyUpdate, PublishedDefinition, SessionView, StoreError};

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
