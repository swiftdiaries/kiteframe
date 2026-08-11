use serde_json::Value;
use thiserror::Error;

use crate::{CatalogEpochId, PolicyRevision, PrincipalId, SessionId};

#[derive(Clone, Debug)]
pub struct CreateSession {
    pub principal: PrincipalId,
    pub working_directory: String,
    pub retention_until: time::OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionView {
    pub id: SessionId,
    pub principal: PrincipalId,
    pub policy_revision: PolicyRevision,
    pub current_catalog_epoch: Option<CatalogEpochId>,
    pub mcp_attached: bool,
}

/// Persistence-facing representation until Task 8 owns typed policy validation.
#[derive(Clone, Debug)]
pub struct PolicyUpdate {
    pub session: SessionId,
    pub expected_revision: PolicyRevision,
    pub idempotency_key: String,
    pub typed_source: Value,
    pub compiled_cedar: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedDefinition {
    pub id: uuid::Uuid,
    pub name: String,
    pub version: u32,
    pub definition_hash: String,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("store migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("session was not found")]
    SessionNotFound,
    #[error("session principal does not match")]
    PrincipalMismatch,
    #[error("session is closed")]
    SessionClosed,
    #[error("conflicting idempotent request")]
    IdempotencyConflict,
    #[error("policy revision conflict")]
    PolicyRevisionConflict,
    #[error("workflow references a catalog binding outside the current session catalog")]
    CatalogScopeViolation,
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
}
