use std::collections::BTreeMap;

use serde_json::{Value, json};
use thiserror::Error;

use crate::dsl::DefinitionKind;
use crate::{
    AnswerQuestion, CancelRun, IdempotencyKey, McpServerConfig, PermissionChoice,
    PermissionRequestId, PolicyRevision, PrincipalId, PublishedDefinition, ResolvePermission,
    RetryRecovery, RunEvent, RunId, RunView, SessionId, SessionPolicy, SessionView, StartRun,
};

pub const AGENT_RUNTIME_UNAVAILABLE: &str = "agent_runtime_unavailable";

#[derive(Clone, Debug)]
pub struct ApplicationContext {
    pub connection_principal: PrincipalId,
    pub session_id: Option<SessionId>,
}

#[derive(Clone, Debug)]
pub struct NewSession {
    pub working_directory: String,
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Clone, Debug)]
pub struct ResumeSession {
    pub session_id: SessionId,
    pub working_directory: String,
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Clone, Debug)]
pub struct CloseSession {
    pub session_id: SessionId,
}

#[derive(Clone, Debug)]
pub struct PublishDefinition {
    pub expected_kind: Option<DefinitionKind>,
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
pub struct ApplicationSession {
    pub view: SessionView,
    pub working_directory: String,
}

#[derive(Clone, Debug)]
pub struct PermissionResponseContext {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub permission_request_id: PermissionRequestId,
    pub idempotency_key: IdempotencyKey,
    options: BTreeMap<String, PermissionChoice>,
}

impl PermissionResponseContext {
    #[must_use]
    pub fn new(
        session_id: SessionId,
        run_id: RunId,
        permission_request_id: PermissionRequestId,
        idempotency_key: IdempotencyKey,
    ) -> Self {
        Self {
            session_id,
            run_id,
            permission_request_id,
            idempotency_key,
            options: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_option(mut self, option_id: impl Into<String>, choice: PermissionChoice) -> Self {
        self.options.insert(option_id.into(), choice);
        self
    }

    pub(crate) fn resolve(self, option_id: &str) -> Result<ApplicationCommand, ProtocolError> {
        let choice = self.options.get(option_id).copied().ok_or_else(|| {
            ProtocolError::InvalidPayload(format!(
                "permission option was not offered by Kiteframe: {option_id}"
            ))
        })?;
        Ok(ApplicationCommand::ResolvePermission(ResolvePermission {
            session_id: self.session_id,
            run_id: self.run_id,
            permission_request_id: self.permission_request_id,
            choice,
            idempotency_key: self.idempotency_key,
        }))
    }
}

#[derive(Clone, Debug)]
pub enum ApplicationCommand {
    NewSession(NewSession),
    ResumeSession(ResumeSession),
    ListSessions,
    CloseSession(CloseSession),
    Publish(PublishDefinition),
    Start(StartRun),
    Get(GetRun),
    Answer(AnswerQuestion),
    ResolvePermission(ResolvePermission),
    Cancel(CancelRun),
    RetryRecovery(RetryRecovery),
    UpdatePolicy(UpdateSessionPolicy),
    ReplayEvents(EventCursor),
}

#[derive(Clone, Debug)]
pub enum ApplicationResult {
    Published(PublishedDefinition),
    Run(RunView),
    PolicyUpdated { revision: PolicyRevision },
    Events(Vec<RunEvent>),
    Session(ApplicationSession),
    Sessions(Vec<ApplicationSession>),
    Closed,
}

pub trait ProtocolAdapter: Send + Sync {
    fn decode_extension(
        &self,
        method: &str,
        params: Value,
    ) -> Result<ApplicationCommand, ProtocolError>;

    fn decode_permission_response(
        &self,
        result: Value,
        context: PermissionResponseContext,
    ) -> Result<ApplicationCommand, ProtocolError>;

    fn encode_session_result(
        &self,
        method: &str,
        result: ApplicationResult,
    ) -> Result<Value, ProtocolError>;

    fn encode_result(&self, result: ApplicationResult) -> Result<Value, ProtocolError>;

    fn encode_extension_result(
        &self,
        method: &str,
        result: ApplicationResult,
    ) -> Result<Value, ProtocolError> {
        encode_extension_application_result(method, result)
    }
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
    #[error("application error: {message}")]
    StableApplication { code: &'static str, message: String },
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
    #[error("this connection has not negotiated an ACP version")]
    NotNegotiated,
    #[error("the ACP permission request was cancelled before a choice was made")]
    PermissionCancelled,
    #[error("session retention has expired")]
    SessionExpired,
    #[error("session working directory does not match the durable session")]
    WorkingDirectoryMismatch,
    #[error("fresh MCP connection configuration is required to resume this session")]
    McpConfigurationRequired,
    #[error("MCP attachment is not enabled for this harness")]
    McpUnavailable,
    #[error("application result is not valid for protocol method {0}")]
    UnexpectedResult(String),
}

impl From<crate::StoreError> for ProtocolError {
    fn from(error: crate::StoreError) -> Self {
        match error {
            crate::StoreError::AgentRuntimeUnavailable => Self::StableApplication {
                code: AGENT_RUNTIME_UNAVAILABLE,
                message: "agent definitions require durable goal lifecycle and runtime authority"
                    .to_owned(),
            },
            error => Self::Application(error.to_string()),
        }
    }
}

impl From<crate::EngineError> for ProtocolError {
    fn from(error: crate::EngineError) -> Self {
        Self::Application(error.to_string())
    }
}

pub(crate) fn encode_protocol_error(error: &ProtocolError) -> agent_client_protocol::Error {
    match error {
        ProtocolError::UnsupportedMethod(_) => agent_client_protocol::Error::method_not_found(),
        ProtocolError::InvalidPayload(_)
        | ProtocolError::UnsupportedVersion(_)
        | ProtocolError::V2Disabled
        | ProtocolError::McpUnavailable => agent_client_protocol::Error::invalid_params(),
        ProtocolError::UnexpectedResult(_) => agent_client_protocol::Error::internal_error(),
        ProtocolError::PermissionCancelled => agent_client_protocol::Error::request_cancelled(),
        ProtocolError::StableApplication { code, .. } => {
            agent_client_protocol::Error::new(-32000, "application request failed")
                .data(json!({"kiteframe_code": code}))
        }
        _ => agent_client_protocol::Error::new(-32000, "application request failed"),
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
        ApplicationCommand::RetryRecovery(request) => Some(&request.session_id),
        ApplicationCommand::NewSession(_)
        | ApplicationCommand::ListSessions
        | ApplicationCommand::Publish(_)
        | ApplicationCommand::UpdatePolicy(_)
        | ApplicationCommand::Get(_)
        | ApplicationCommand::ReplayEvents(_) => None,
    }
}

pub(crate) fn encode_application_result(result: ApplicationResult) -> Result<Value, ProtocolError> {
    encode_application_result_with_kind(result, false)
}

pub(crate) fn encode_extension_application_result(
    method: &str,
    result: ApplicationResult,
) -> Result<Value, ProtocolError> {
    encode_application_result_with_kind(result, method == "_kiteframe/definition/publish")
}

fn encode_application_result_with_kind(
    result: ApplicationResult,
    include_definition_kind: bool,
) -> Result<Value, ProtocolError> {
    Ok(match result {
        ApplicationResult::Published(definition) => {
            let mut response = json!({
                "id": definition.id,
                "name": definition.name,
                "version": definition.version,
                "definitionHash": definition.definition_hash,
            });
            if include_definition_kind {
                response["kind"] = json!(definition.kind);
            }
            response
        }
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
        ApplicationResult::Session(_)
        | ApplicationResult::Sessions(_)
        | ApplicationResult::Closed => {
            return Err(ProtocolError::UnexpectedResult(
                "extension result".to_owned(),
            ));
        }
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
