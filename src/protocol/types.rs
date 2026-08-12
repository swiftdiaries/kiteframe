use serde::Deserialize;
use thiserror::Error;

use crate::{AnswerQuestion, CancelRun, IdempotencyKey, PolicyRevision, PrincipalId, PublishedDefinition, ResolvePermission, RunEvent, RunId, RunView, SessionId, SessionPolicy, StartRun};

#[derive(Clone, Debug)]
pub struct ApplicationContext {
    pub principal: PrincipalId,
    pub session_id: Option<SessionId>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GetRun {
    pub run_id: RunId,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EventCursor {
    pub run_id: RunId,
    pub after: u64,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PublishWorkflow {
    pub source: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateSessionPolicy {
    pub expected_revision: PolicyRevision,
    pub idempotency_key: IdempotencyKey,
    pub policy: SessionPolicy,
}

#[derive(Clone, Debug)]
pub enum ApplicationCommand {
    ListSessions,
    CloseSession,
    Publish(PublishWorkflow),
    UpdatePolicy(UpdateSessionPolicy),
    Start(StartRun),
    Get(GetRun),
    Answer(AnswerQuestion),
    ResolvePermission(ResolvePermission),
    Cancel(CancelRun),
    ReplayEvents(EventCursor),
}

#[derive(Clone, Debug)]
pub enum ApplicationResult {
    Sessions(Vec<crate::SessionView>),
    Published(PublishedDefinition),
    PolicyUpdated { revision: PolicyRevision },
    Run(RunView),
    Events(Vec<RunEvent>),
    Ack,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("an authenticated session is required")]
    MissingSession,
    #[error("command session does not match the authenticated connection")]
    SessionMismatch,
    #[error("application error: {0}")]
    Application(String),
    #[error("invalid command payload: {0}")]
    InvalidPayload(String),
    #[error("unsupported application command")]
    Unsupported,
}

impl From<crate::StoreError> for ProtocolError {
    fn from(error: crate::StoreError) -> Self { Self::Application(error.to_string()) }
}

impl From<crate::EngineError> for ProtocolError {
    fn from(error: crate::EngineError) -> Self { Self::Application(error.to_string()) }
}

pub fn command_session(command: &ApplicationCommand) -> Option<&SessionId> {
    match command {
        ApplicationCommand::ListSessions => None,
        ApplicationCommand::CloseSession => None,
        ApplicationCommand::Start(request) => Some(&request.session_id),
        ApplicationCommand::Answer(request) => Some(&request.session_id),
        ApplicationCommand::ResolvePermission(request) => Some(&request.session_id),
        ApplicationCommand::Cancel(request) => Some(&request.session_id),
        ApplicationCommand::Publish(_) | ApplicationCommand::UpdatePolicy(_) | ApplicationCommand::Get(_) | ApplicationCommand::ReplayEvents(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{IdempotencyKey, SessionId, StartRun};
    use serde_json::json;
    use uuid::Uuid;

    use super::{ApplicationCommand, command_session};

    #[test]
    fn start_command_exposes_its_session_for_connection_binding() {
        let session = SessionId::new(Uuid::now_v7());
        let command = ApplicationCommand::Start(StartRun {
            session_id: session.clone(), workflow_name: "w".to_owned(), version: None,
            input: json!({}), idempotency_key: IdempotencyKey::new("start").unwrap(),
        });
        assert_eq!(command_session(&command), Some(&session));
    }
}
