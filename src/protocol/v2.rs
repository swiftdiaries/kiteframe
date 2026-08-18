use agent_client_protocol::schema::{ProtocolVersion, v2 as acp};
use serde::Deserialize;
use serde_json::Value;

use crate::dsl::DefinitionKind;
use crate::{
    AnswerQuestion, CancelRun, IdempotencyKey, McpServerConfig, PolicyRevision, RetryRecovery,
    RunId, Secret, SessionId, SessionPolicy, StartRun,
};

use super::{
    ApplicationCommand, ApplicationResult, CloseSession, EventCursor, GetRun, NewSession,
    PermissionResponseContext, ProtocolAdapter, ProtocolError, PublishDefinition, ResumeSession,
    UpdateSessionPolicy, encode_application_result,
};

/// Draft ACP v2 adapter. Constructing the adapter is compile-time gated, and
/// initialization additionally requires the deployment's runtime opt-in.
#[derive(Clone, Copy, Debug)]
pub struct V2Adapter {
    runtime_enabled: bool,
}

impl V2Adapter {
    #[must_use]
    pub const fn new(runtime_enabled: bool) -> Self {
        Self { runtime_enabled }
    }

    pub fn initialize(&self, params: Value) -> Result<Value, ProtocolError> {
        if !self.runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        let request: acp::InitializeRequest = decode(params)?;
        if request.protocol_version != ProtocolVersion::V2 {
            return Err(ProtocolError::UnsupportedVersion(
                request.protocol_version.as_u16(),
            ));
        }

        let capabilities = acp::AgentCapabilities::new().session(acp::SessionCapabilities::new());
        let mut response = acp::InitializeResponse::new(
            ProtocolVersion::V2,
            acp::Implementation::new("kiteframe", crate::VERSION),
        )
        .capabilities(capabilities);
        response.meta = request.meta;
        serde_json::to_value(response)
            .map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
    }

    pub fn decode_session(
        &self,
        method: &str,
        params: Value,
    ) -> Result<ApplicationCommand, ProtocolError> {
        if !self.runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        match method {
            "session/new" => {
                let request: acp::NewSessionRequest = decode(params)?;
                ensure_empty_directories(&request.additional_directories)?;
                Ok(ApplicationCommand::NewSession(NewSession {
                    working_directory: path_string(request.cwd.as_ref())?,
                    mcp_servers: mcp_servers(request.mcp_servers)?,
                }))
            }
            "session/list" => {
                let request: acp::ListSessionsRequest = decode(params)?;
                if request.cwd.is_some() || request.cursor.is_some() {
                    return Err(ProtocolError::InvalidPayload(
                        "session list filtering and pagination are not supported".to_owned(),
                    ));
                }
                Ok(ApplicationCommand::ListSessions)
            }
            "session/resume" => {
                let request: acp::ResumeSessionRequest = decode(params)?;
                ensure_empty_directories(&request.additional_directories)?;
                if request.replay_from.is_some() {
                    return Err(ProtocolError::InvalidPayload(
                        "ACP conversation replay is not a workflow event cursor".to_owned(),
                    ));
                }
                Ok(ApplicationCommand::ResumeSession(ResumeSession {
                    session_id: session_id(request.session_id)?,
                    working_directory: path_string(request.cwd.as_ref())?,
                    mcp_servers: mcp_servers(request.mcp_servers)?,
                }))
            }
            "session/close" => {
                let request: acp::CloseSessionRequest = decode(params)?;
                Ok(ApplicationCommand::CloseSession(CloseSession {
                    session_id: session_id(request.session_id)?,
                }))
            }
            _ => Err(ProtocolError::UnsupportedMethod(method.to_owned())),
        }
    }

    pub fn decode_permission(
        &self,
        params: Value,
    ) -> Result<acp::RequestPermissionRequest, ProtocolError> {
        if !self.runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        decode(params)
    }

    #[must_use]
    pub fn encode_error(&self, error: &ProtocolError) -> agent_client_protocol::Error {
        super::types::encode_protocol_error(error)
    }
}

impl ProtocolAdapter for V2Adapter {
    fn decode_extension(
        &self,
        method: &str,
        params: Value,
    ) -> Result<ApplicationCommand, ProtocolError> {
        if !self.runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        match method {
            "_kiteframe/workflow/publish" => decode(params)
                .map(|dto: PublishDto| dto.into_command(Some(DefinitionKind::Workflow))),
            "_kiteframe/definition/publish" => {
                decode(params).map(|dto: PublishDto| dto.into_command(None))
            }
            "_kiteframe/run/start" => decode(params).map(StartDto::into_command),
            "_kiteframe/run/get" => decode(params).map(GetDto::into_command),
            "_kiteframe/run/answer" => decode(params).map(AnswerDto::into_command),
            "_kiteframe/run/cancel" => decode(params).map(CancelDto::into_command),
            "_kiteframe/run/recovery/retry" => decode(params).map(RetryRecoveryDto::into_command),
            "_kiteframe/session/policy/update" => decode(params).map(UpdatePolicyDto::into_command),
            "_kiteframe/run/event" => decode(params).map(EventCursorDto::into_command),
            _ => Err(ProtocolError::UnsupportedMethod(method.to_owned())),
        }
    }

    fn decode_permission_response(
        &self,
        result: Value,
        context: PermissionResponseContext,
    ) -> Result<ApplicationCommand, ProtocolError> {
        if !self.runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        let response: acp::RequestPermissionResponse = decode(result)?;
        match response.outcome {
            acp::RequestPermissionOutcome::Selected(selected) => {
                context.resolve(selected.option_id.0.as_ref())
            }
            acp::RequestPermissionOutcome::Cancelled => Err(ProtocolError::PermissionCancelled),
            _ => Err(ProtocolError::InvalidPayload(
                "unsupported ACP permission outcome".to_owned(),
            )),
        }
    }

    fn encode_session_result(
        &self,
        method: &str,
        result: ApplicationResult,
    ) -> Result<Value, ProtocolError> {
        if !self.runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        let response = match (method, result) {
            ("session/new", ApplicationResult::Session(session)) => {
                serde_json::to_value(acp::NewSessionResponse::new(acp::SessionId::new(
                    session.view.id.as_uuid().to_string(),
                )))
            }
            ("session/list", ApplicationResult::Sessions(sessions)) => {
                let sessions = sessions
                    .into_iter()
                    .map(|session| {
                        acp::SessionInfo::new(
                            acp::SessionId::new(session.view.id.as_uuid().to_string()),
                            acp::AbsolutePath::new(std::path::PathBuf::from(
                                session.working_directory,
                            )),
                        )
                    })
                    .collect();
                serde_json::to_value(acp::ListSessionsResponse::new(sessions))
            }
            ("session/resume", ApplicationResult::Session(_)) => {
                serde_json::to_value(acp::ResumeSessionResponse::new())
            }
            ("session/close", ApplicationResult::Closed) => {
                serde_json::to_value(acp::CloseSessionResponse::new())
            }
            _ => return Err(ProtocolError::UnexpectedResult(method.to_owned())),
        };
        response.map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
    }

    fn encode_result(&self, result: ApplicationResult) -> Result<Value, ProtocolError> {
        if !self.runtime_enabled {
            return Err(ProtocolError::V2Disabled);
        }
        encode_application_result(result)
    }
}

fn decode<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ProtocolError> {
    serde_json::from_value(params).map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
}

fn session_id(id: acp::SessionId) -> Result<SessionId, ProtocolError> {
    uuid::Uuid::parse_str(&id.0)
        .map(SessionId::new)
        .map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
}

fn path_string(path: &std::path::Path) -> Result<String, ProtocolError> {
    if !path.is_absolute() {
        return Err(ProtocolError::InvalidPayload(
            "working directory must be absolute".to_owned(),
        ));
    }
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ProtocolError::InvalidPayload("working directory is not UTF-8".to_owned()))
}

fn ensure_empty_directories<A>(additional_directories: &[A]) -> Result<(), ProtocolError> {
    if !additional_directories.is_empty() {
        return Err(ProtocolError::InvalidPayload(
            "additional directories are not supported".to_owned(),
        ));
    }
    Ok(())
}

fn mcp_servers(servers: Vec<acp::McpServer>) -> Result<Vec<McpServerConfig>, ProtocolError> {
    servers
        .into_iter()
        .map(|server| match server {
            acp::McpServer::Stdio(server) => Ok(McpServerConfig::Stdio {
                alias: server.name,
                program: server
                    .command
                    .0
                    .to_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        ProtocolError::InvalidPayload("MCP command is not UTF-8".to_owned())
                    })?,
                args: server.args,
                env: server
                    .env
                    .into_iter()
                    .map(|entry| (entry.name, Secret::new(entry.value)))
                    .collect(),
            }),
            acp::McpServer::Http(server) => Ok(McpServerConfig::StreamableHttp {
                alias: server.name,
                url: server.url.parse().map_err(|error| {
                    ProtocolError::InvalidPayload(format!("invalid MCP URL: {error}"))
                })?,
                headers: server
                    .headers
                    .into_iter()
                    .map(|header| (header.name, Secret::new(header.value)))
                    .collect(),
            }),
            acp::McpServer::Other(_) => Err(ProtocolError::InvalidPayload(
                "unsupported MCP server configuration".to_owned(),
            )),
            _ => Err(ProtocolError::InvalidPayload(
                "unsupported MCP server configuration".to_owned(),
            )),
        })
        .collect()
}

type Meta = Option<serde_json::Map<String, Value>>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublishDto {
    source: String,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

impl PublishDto {
    fn into_command(self, expected_kind: Option<DefinitionKind>) -> ApplicationCommand {
        ApplicationCommand::Publish(PublishDefinition {
            expected_kind,
            source: self.source,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartDto {
    session_id: SessionId,
    workflow_name: String,
    version: Option<u32>,
    input: Value,
    idempotency_key: IdempotencyKey,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

impl StartDto {
    fn into_command(self) -> ApplicationCommand {
        ApplicationCommand::Start(StartRun {
            session_id: self.session_id,
            workflow_name: self.workflow_name,
            version: self.version,
            input: self.input,
            idempotency_key: self.idempotency_key,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GetDto {
    run_id: RunId,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

impl GetDto {
    fn into_command(self) -> ApplicationCommand {
        ApplicationCommand::Get(GetRun {
            run_id: self.run_id,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AnswerDto {
    session_id: SessionId,
    run_id: RunId,
    question_id: uuid::Uuid,
    answer: Value,
    idempotency_key: IdempotencyKey,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

impl AnswerDto {
    fn into_command(self) -> ApplicationCommand {
        ApplicationCommand::Answer(AnswerQuestion {
            session_id: self.session_id,
            run_id: self.run_id,
            question_id: self.question_id,
            answer: self.answer,
            idempotency_key: self.idempotency_key,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelDto {
    session_id: SessionId,
    run_id: RunId,
    reason: Option<String>,
    idempotency_key: IdempotencyKey,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RetryRecoveryDto {
    session_id: SessionId,
    run_id: RunId,
    tool_call_id: uuid::Uuid,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

impl RetryRecoveryDto {
    fn into_command(self) -> ApplicationCommand {
        ApplicationCommand::RetryRecovery(RetryRecovery {
            session_id: self.session_id,
            run_id: self.run_id,
            tool_call_id: self.tool_call_id,
        })
    }
}

impl CancelDto {
    fn into_command(self) -> ApplicationCommand {
        ApplicationCommand::Cancel(CancelRun {
            session_id: self.session_id,
            run_id: self.run_id,
            reason: self.reason,
            idempotency_key: self.idempotency_key,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdatePolicyDto {
    expected_revision: PolicyRevision,
    idempotency_key: IdempotencyKey,
    policy: SessionPolicy,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

impl UpdatePolicyDto {
    fn into_command(self) -> ApplicationCommand {
        ApplicationCommand::UpdatePolicy(UpdateSessionPolicy {
            expected_revision: self.expected_revision,
            idempotency_key: self.idempotency_key,
            policy: self.policy,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventCursorDto {
    run_id: RunId,
    after: i64,
    limit: u32,
    #[serde(default, rename = "_meta")]
    _meta: Meta,
}

impl EventCursorDto {
    fn into_command(self) -> ApplicationCommand {
        ApplicationCommand::ReplayEvents(EventCursor {
            run_id: self.run_id,
            after: self.after,
            limit: self.limit,
        })
    }
}
