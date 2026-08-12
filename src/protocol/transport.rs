use http::{HeaderMap, header::AUTHORIZATION};
use serde::Deserialize;
use serde_json::Value;
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::PrincipalId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Stdio,
    WebSocket,
    StreamableHttp,
}

#[derive(Clone, Debug)]
pub struct AuthenticatedConnection {
    pub principal: PrincipalId,
    pub transport: TransportKind,
}

pub trait ConnectionAuthenticator: Send + Sync {
    fn authenticate(&self, headers: &HeaderMap) -> Result<PrincipalId, AuthError>;
}

#[derive(Clone, Debug)]
pub struct StaticBearerAuthenticator {
    token: String,
    principal: PrincipalId,
}

impl StaticBearerAuthenticator {
    #[must_use]
    pub fn new(token: String, principal: PrincipalId) -> Self {
        Self { token, principal }
    }
}

impl ConnectionAuthenticator for StaticBearerAuthenticator {
    fn authenticate(&self, headers: &HeaderMap) -> Result<PrincipalId, AuthError> {
        let value = headers
            .get(AUTHORIZATION)
            .ok_or(AuthError::MissingAuthorization)?
            .to_str()
            .map_err(|_| AuthError::InvalidAuthorization)?;
        let token = value
            .strip_prefix("Bearer ")
            .ok_or(AuthError::InvalidAuthorization)?;
        if self.token.as_bytes().ct_eq(token.as_bytes()).unwrap_u8() != 1 {
            return Err(AuthError::Unauthorized);
        }
        Ok(self.principal.clone())
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("authorization header is required")]
    MissingAuthorization,
    #[error("authorization header must use Bearer authentication")]
    InvalidAuthorization,
    #[error("authorization is not accepted")]
    Unauthorized,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NdjsonRequest {
    pub session_id: crate::SessionId,
    pub method: String,
    pub payload: Value,
}

impl NdjsonRequest {
    pub fn into_command(
        self,
    ) -> Result<(crate::SessionId, crate::ApplicationCommand), crate::ProtocolError> {
        Ok((
            self.session_id,
            super::v1::V1Adapter::decode_extension(&self.method, self.payload)?,
        ))
    }
}

pub async fn serve_ndjson<R, W>(
    reader: R,
    mut writer: W,
    application: &crate::Application,
    connection: &AuthenticatedConnection,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = reader.lines();
    let mut initialized = false;
    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<NdjsonRequest>(&line) {
            Ok(request) if request.method == "initialize" => {
                if initialized {
                    Err("connection is already initialized".to_owned())
                } else {
                    initialized = true;
                    Ok(serde_json::json!({"kind":"initialized", "version": crate::VERSION}))
                }
            }
            Ok(_) if !initialized => Err("connection has not been initialized".to_owned()),
            Ok(request) => match request.into_command() {
                Ok((session_id, command)) => application
                    .dispatch(
                        &crate::ApplicationContext {
                            principal: connection.principal.clone(),
                            session_id: Some(session_id),
                        },
                        command,
                    )
                    .await
                    .map(application_result_json)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(format!("invalid NDJSON request: {error}")),
        };
        let payload = match response {
            Ok(value) => serde_json::json!({"ok": true, "result": value}),
            Err(error) => serde_json::json!({"ok": false, "error": error}),
        };
        writer
            .write_all(&serde_json::to_vec(&payload).expect("JSON value serializes"))
            .await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}

fn application_result_json(result: crate::ApplicationResult) -> Value {
    match result {
        crate::ApplicationResult::Published(definition) => serde_json::json!({"kind":"published", "id":definition.id, "name":definition.name, "version":definition.version, "definition_hash":definition.definition_hash}),
        crate::ApplicationResult::PolicyUpdated { revision } => serde_json::json!({"kind":"policy_updated", "revision":revision.get()}),
        crate::ApplicationResult::Run(run) => serde_json::json!({
            "kind": "run", "id": run.id.as_uuid(), "session_id": run.session_id.as_uuid(),
            "workflow_name": run.definition.name, "version": run.definition.version,
            "state": run_state_name(run.state), "revision": run.revision,
        }),
        crate::ApplicationResult::Events(events) => {
            serde_json::json!({"kind":"events", "events": events.into_iter().map(|event| serde_json::json!({"sequence":event.sequence,"kind":event.kind,"metadata":event.metadata,"created_at":event.created_at.to_string()})).collect::<Vec<_>>() })
        }
        crate::ApplicationResult::Sessions(sessions) => {
            serde_json::json!({"kind":"sessions", "sessions": sessions.into_iter().map(|session| serde_json::json!({"id":session.id.as_uuid(),"principal":session.principal.as_str(),"policy_revision":session.policy_revision.get(),"mcp_attached":session.mcp_attached})).collect::<Vec<_>>() })
        }
        crate::ApplicationResult::Ack => serde_json::json!({"kind":"ack"}),
    }
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
    use super::{ConnectionAuthenticator, StaticBearerAuthenticator};
    use crate::PrincipalId;
    use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    #[test]
    fn static_bearer_authenticator_accepts_only_the_configured_token() {
        let auth =
            StaticBearerAuthenticator::new("token".to_owned(), PrincipalId::new("alice").unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token"));
        assert_eq!(auth.authenticate(&headers).unwrap().as_str(), "alice");
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert!(auth.authenticate(&headers).is_err());
    }

    #[test]
    fn ndjson_request_keeps_transport_session_separate_from_command_payload() {
        let request: super::NdjsonRequest = serde_json::from_value(serde_json::json!({
            "session_id":"019ff3d8-9d86-7de0-9070-0f0185d5f413", "method":"_kiteframe/run/get",
            "payload":{"run_id":"019ff3d8-9d86-7de0-9070-0f0185d5f413"}
        }))
        .unwrap();
        let (session, command) = request.into_command().unwrap();
        assert_eq!(
            session.as_uuid().to_string(),
            "019ff3d8-9d86-7de0-9070-0f0185d5f413"
        );
        assert!(matches!(command, crate::ApplicationCommand::Get(_)));
    }
}
