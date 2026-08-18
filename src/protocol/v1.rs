use std::path::Path;

use agent_client_protocol::schema::{ProtocolVersion, v1 as acp};
use serde::Deserialize;
use serde_json::Value;

use crate::dsl::DefinitionKind;
use crate::{
    AnswerQuestion, CancelRun, IdempotencyKey, McpServerConfig, PolicyRevision, RetryRecovery,
    RunId, Secret, SessionId, SessionPolicy, StartRun,
};

use super::{
    ApplicationCommand, ApplicationResult, EventCursor, GetRun, NewSession,
    PermissionResponseContext, ProtocolAdapter, ProtocolError, PublishDefinition, ResumeSession,
    UpdateSessionPolicy, encode_application_result,
};

/// Stable ACP v1 adapter. Standard ACP messages use SDK DTOs; private wire DTOs
/// below exist only to translate Kiteframe extensions into application commands.
#[derive(Clone, Copy, Debug, Default)]
pub struct V1Adapter;

impl V1Adapter {
    pub fn initialize(&self, params: Value) -> Result<Value, ProtocolError> {
        let request: acp::InitializeRequest = decode(params)?;
        if request.protocol_version != ProtocolVersion::V1 {
            return Err(ProtocolError::UnsupportedVersion(
                request.protocol_version.as_u16(),
            ));
        }

        let mut response = acp::InitializeResponse::new(ProtocolVersion::V1)
            .agent_capabilities(acp::AgentCapabilities::new().load_session(true))
            .agent_info(acp::Implementation::new("kiteframe", crate::VERSION));
        response.meta = request.meta;
        serde_json::to_value(response)
            .map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
    }

    pub fn decode_session(
        &self,
        method: &str,
        params: Value,
    ) -> Result<ApplicationCommand, ProtocolError> {
        match method {
            "session/new" => {
                let request: acp::NewSessionRequest = decode(params)?;
                ensure_empty_directories(&request.additional_directories)?;
                Ok(ApplicationCommand::NewSession(NewSession {
                    working_directory: path_string(&request.cwd)?,
                    mcp_servers: mcp_servers(request.mcp_servers)?,
                }))
            }
            "session/load" => {
                let request: acp::LoadSessionRequest = decode(params)?;
                ensure_empty_directories(&request.additional_directories)?;
                Ok(ApplicationCommand::ResumeSession(ResumeSession {
                    session_id: session_id(request.session_id)?,
                    working_directory: path_string(&request.cwd)?,
                    mcp_servers: mcp_servers(request.mcp_servers)?,
                }))
            }
            _ => Err(ProtocolError::UnsupportedMethod(method.to_owned())),
        }
    }

    pub fn decode_permission(
        &self,
        params: Value,
    ) -> Result<acp::RequestPermissionRequest, ProtocolError> {
        decode(params)
    }

    pub fn decode_extension(
        method: &str,
        params: Value,
    ) -> Result<ApplicationCommand, ProtocolError> {
        <Self as ProtocolAdapter>::decode_extension(&Self, method, params)
    }

    #[must_use]
    pub fn encode_error(error: &ProtocolError) -> agent_client_protocol::Error {
        super::types::encode_protocol_error(error)
    }
}

impl ProtocolAdapter for V1Adapter {
    fn decode_extension(
        &self,
        method: &str,
        params: Value,
    ) -> Result<ApplicationCommand, ProtocolError> {
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
        let response = match (method, result) {
            ("session/new", ApplicationResult::Session(session)) => {
                serde_json::to_value(acp::NewSessionResponse::new(acp::SessionId::new(
                    session.view.id.as_uuid().to_string(),
                )))
            }
            ("session/load", ApplicationResult::Session(_)) => {
                serde_json::to_value(acp::LoadSessionResponse::new())
            }
            _ => return Err(ProtocolError::UnexpectedResult(method.to_owned())),
        };
        response.map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
    }

    fn encode_result(&self, result: ApplicationResult) -> Result<Value, ProtocolError> {
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

fn path_string(path: &Path) -> Result<String, ProtocolError> {
    if !path.is_absolute() {
        return Err(ProtocolError::InvalidPayload(
            "working directory must be absolute".to_owned(),
        ));
    }
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ProtocolError::InvalidPayload("working directory is not UTF-8".to_owned()))
}

fn ensure_empty_directories(
    additional_directories: &[std::path::PathBuf],
) -> Result<(), ProtocolError> {
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
            acp::McpServer::Sse(_) => Err(ProtocolError::InvalidPayload(
                "SSE MCP servers are not supported; use streamable HTTP".to_owned(),
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
