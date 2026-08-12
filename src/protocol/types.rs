use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    AnswerQuestion, CancelRun, IdempotencyKey, PolicyRevision, PrincipalId, PublishedDefinition,
    ResolvePermission, RunEvent, RunId, RunView, SessionId, SessionPolicy, SessionView, StartRun,
};

#[derive(Clone, Debug)]
pub struct ApplicationContext {
    pub connection_principal: PrincipalId,
    pub session_id: Option<SessionId>,
}

#[derive(Clone, Debug)]
pub struct NewSession {
    pub working_directory: String,
}

#[derive(Clone, Debug)]
pub struct ResumeSession {
    pub session_id: SessionId,
    pub working_directory: String,
}

#[derive(Clone, Debug)]
pub struct CloseSession {
    pub session_id: SessionId,
}

#[derive(Clone, Debug)]
pub struct PublishWorkflow {
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct GetRun {
    pub run_id: RunId,
}

#[derive(Clone, Debug)]
pub struct EventCursor {
    pub run_id: RunId,
    pub after: i64,
    pub limit: u32,
}

#[derive(Clone, Debug)]
pub struct UpdateSessionPolicy {
    pub expected_revision: PolicyRevision,
    pub idempotency_key: IdempotencyKey,
    pub policy: SessionPolicy,
}

#[derive(Clone, Debug)]
pub enum ApplicationCommand {
    NewSession(NewSession),
    ResumeSession(ResumeSession),
    ListSessions,
    CloseSession(CloseSession),
    Publish(PublishWorkflow),
    Start(StartRun),
    Get(GetRun),
    Answer(AnswerQuestion),
    ResolvePermission(ResolvePermission),
    Cancel(CancelRun),
    UpdatePolicy(UpdateSessionPolicy),
    ReplayEvents(EventCursor),
}

#[derive(Clone, Debug)]
pub enum ApplicationResult {
    Published(PublishedDefinition),
    Run(RunView),
    PolicyUpdated { revision: PolicyRevision },
    Events(Vec<RunEvent>),
    Session(SessionView),
    Sessions(Vec<SessionView>),
    Closed,
}

pub trait ProtocolAdapter: Send + Sync {
    fn decode_extension(
        &self,
        method: &str,
        params: Value,
    ) -> Result<ApplicationCommand, ProtocolError>;

    fn encode_result(&self, result: ApplicationResult) -> Result<Value, ProtocolError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegotiatedProtocol {
    V1,
    #[cfg(feature = "acp-v2")]
    V2,
}

#[derive(Clone, Debug)]
pub struct ProtocolNegotiation {
    selected: Option<NegotiatedProtocol>,
    _v2_runtime_enabled: bool,
}

impl ProtocolNegotiation {
    #[must_use]
    pub const fn new(v2_runtime_enabled: bool) -> Self {
        Self {
            selected: None,
            _v2_runtime_enabled: v2_runtime_enabled,
        }
    }

    pub fn select_v1(&mut self) -> Result<(), ProtocolError> {
        self.select(NegotiatedProtocol::V1)
    }

    #[cfg(feature = "acp-v2")]
    pub fn select_v2(&mut self) -> Result<(), ProtocolError> {
        if !self._v2_runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        self.select(NegotiatedProtocol::V2)
    }

    #[must_use]
    pub const fn selected(&self) -> Option<NegotiatedProtocol> {
        self.selected
    }

    fn select(&mut self, version: NegotiatedProtocol) -> Result<(), ProtocolError> {
        if self.selected.is_some() {
            return Err(ProtocolError::AlreadyNegotiated);
        }
        self.selected = Some(version);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("an authenticated session is required")]
    MissingSession,
    #[error("command session does not match the authenticated connection")]
    SessionMismatch,
    #[error("application error: {0}")]
    Application(String),
    #[error("invalid protocol payload: {0}")]
    InvalidPayload(String),
    #[error("unsupported protocol method: {0}")]
    UnsupportedMethod(String),
    #[error("unsupported ACP protocol version: {0}")]
    UnsupportedVersion(u16),
    #[error("ACP v2 support is not enabled at runtime")]
    V2Disabled,
    #[error("this connection has already negotiated an ACP version")]
    AlreadyNegotiated,
}

impl From<crate::StoreError> for ProtocolError {
    fn from(error: crate::StoreError) -> Self {
        Self::Application(error.to_string())
    }
}

impl From<crate::EngineError> for ProtocolError {
    fn from(error: crate::EngineError) -> Self {
        Self::Application(error.to_string())
    }
}

pub(crate) fn command_session(command: &ApplicationCommand) -> Option<&SessionId> {
    match command {
        ApplicationCommand::ResumeSession(request) => Some(&request.session_id),
        ApplicationCommand::CloseSession(request) => Some(&request.session_id),
        ApplicationCommand::Start(request) => Some(&request.session_id),
        ApplicationCommand::Answer(request) => Some(&request.session_id),
        ApplicationCommand::ResolvePermission(request) => Some(&request.session_id),
        ApplicationCommand::Cancel(request) => Some(&request.session_id),
        ApplicationCommand::NewSession(_)
        | ApplicationCommand::ListSessions
        | ApplicationCommand::Publish(_)
        | ApplicationCommand::UpdatePolicy(_)
        | ApplicationCommand::Get(_)
        | ApplicationCommand::ReplayEvents(_) => None,
    }
}

pub(crate) fn encode_application_result(result: ApplicationResult) -> Value {
    match result {
        ApplicationResult::Published(definition) => json!({
            "id": definition.id,
            "name": definition.name,
            "version": definition.version,
            "definitionHash": definition.definition_hash,
        }),
        ApplicationResult::PolicyUpdated { revision } => {
            json!({ "revision": revision.get() })
        }
        ApplicationResult::Run(run) => json!({
            "id": run.id.as_uuid(),
            "sessionId": run.session_id.as_uuid(),
            "workflowName": run.definition.name,
            "version": run.definition.version,
            "state": run_state_name(run.state),
            "revision": run.revision,
        }),
        ApplicationResult::Events(events) => json!({
            "events": events.into_iter().map(|event| json!({
                "sequence": event.sequence,
                "kind": event.kind,
                "metadata": event.metadata,
                "createdAt": event.created_at.to_string(),
            })).collect::<Vec<_>>()
        }),
        ApplicationResult::Session(session) => session_json(session),
        ApplicationResult::Sessions(sessions) => json!({
            "sessions": sessions.into_iter().map(session_json).collect::<Vec<_>>()
        }),
        ApplicationResult::Closed => json!({}),
    }
}

fn session_json(session: SessionView) -> Value {
    json!({
        "sessionId": session.id.as_uuid(),
        "policyRevision": session.policy_revision.get(),
        "catalogEpochId": session.current_catalog_epoch.map(|id| *id.as_uuid()),
        "mcpAttached": session.mcp_attached,
    })
}

fn run_state_name(state: crate::RunState) -> &'static str {
    match state {
        crate::RunState::Runnable => "runnable",
        crate::RunState::Running => "running",
        crate::RunState::WaitingForAnswer => "waiting_for_answer",
        crate::RunState::WaitingForPermission => "waiting_for_permission",
        crate::RunState::OutcomeUnknown => "outcome_unknown",
        crate::RunState::Completed => "completed",
        crate::RunState::Failed => "failed",
        crate::RunState::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use crate::{IdempotencyKey, SessionId, StartRun};

    use super::{ApplicationCommand, command_session};

    #[test]
    fn start_command_exposes_its_session_for_connection_binding() {
        let session = SessionId::new(Uuid::now_v7());
        let command = ApplicationCommand::Start(StartRun {
            session_id: session.clone(),
            workflow_name: "w".to_owned(),
            version: None,
            input: json!({}),
            idempotency_key: IdempotencyKey::new("start").unwrap(),
        });
        assert_eq!(command_session(&command), Some(&session));
    }
}
