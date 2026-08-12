use serde_json::Value;
use thiserror::Error;

use crate::{CatalogEpochId, FrameId, IdempotencyKey, PolicyRevision, PrincipalId, RunId, SessionId};

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

#[derive(Clone, Debug)]
pub struct StartRun {
    pub session_id: SessionId,
    pub workflow_name: String,
    pub version: Option<u32>,
    pub input: Value,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunView {
    pub id: RunId,
    pub session_id: SessionId,
    pub definition: PublishedDefinition,
    pub root_frame_id: FrameId,
    pub state: RunState,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    Runnable,
    Running,
    WaitingForAnswer,
    WaitingForPermission,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelAttempt {
    pub frame_id: FrameId,
    pub attempt: u32,
    pub revision: u64,
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
    #[error("workflow definition was not found")]
    DefinitionNotFound,
    #[error("frame was not found")]
    FrameNotFound,
    #[error("execution revision conflict")]
    RevisionConflict,
    #[error("stored data is invalid: {0}")]
    InvalidData(String),
}
