use http::{HeaderMap, header::AUTHORIZATION};
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::{NegotiatedProtocol, ProtocolAdapter, ProtocolNegotiation};
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

#[derive(Debug)]
pub struct ProtocolConnection {
    authenticated: AuthenticatedConnection,
    negotiation: Mutex<ProtocolNegotiation>,
    session_id: Mutex<Option<crate::SessionId>>,
    _v2_runtime_enabled: bool,
}

impl ProtocolConnection {
    #[must_use]
    pub fn new(authenticated: AuthenticatedConnection, v2_runtime_enabled: bool) -> Self {
        Self {
            authenticated,
            negotiation: Mutex::new(ProtocolNegotiation::new(v2_runtime_enabled)),
            session_id: Mutex::new(None),
            _v2_runtime_enabled: v2_runtime_enabled,
        }
    }

    pub fn initialize(&self, params: Value) -> Result<Value, crate::ProtocolError> {
        let version = params
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| {
                crate::ProtocolError::InvalidPayload(
                    "initialize protocolVersion must be an unsigned integer".to_owned(),
                )
            })?;
        let mut negotiation = self.negotiation.lock().expect("protocol lock poisoned");
        if negotiation.selected().is_some() {
            return Err(crate::ProtocolError::AlreadyNegotiated);
        }
        match version {
            1 => {
                let response = super::V1Adapter.initialize(params)?;
                negotiation.select_v1()?;
                Ok(response)
            }
            #[cfg(feature = "acp-v2")]
            2 => {
                let response =
                    super::V2Adapter::new(self._v2_runtime_enabled).initialize(params)?;
                negotiation.select_v2()?;
                Ok(response)
            }
            other => Err(crate::ProtocolError::UnsupportedVersion(other)),
        }
    }

    #[must_use]
    pub fn negotiated_protocol(&self) -> Option<NegotiatedProtocol> {
        self.negotiation
            .lock()
            .expect("protocol lock poisoned")
            .selected()
    }

    pub fn decode_session(
        &self,
        method: &str,
        params: Value,
    ) -> Result<crate::ApplicationCommand, crate::ProtocolError> {
        match self.selected()? {
            NegotiatedProtocol::V1 => super::V1Adapter.decode_session(method, params),
            #[cfg(feature = "acp-v2")]
            NegotiatedProtocol::V2 => {
                super::V2Adapter::new(self._v2_runtime_enabled).decode_session(method, params)
            }
        }
    }

    pub fn decode_extension(
        &self,
        method: &str,
        params: Value,
    ) -> Result<crate::ApplicationCommand, crate::ProtocolError> {
        match self.selected()? {
            NegotiatedProtocol::V1 => super::V1Adapter.decode_extension(method, params),
            #[cfg(feature = "acp-v2")]
            NegotiatedProtocol::V2 => {
                super::V2Adapter::new(self._v2_runtime_enabled).decode_extension(method, params)
            }
        }
    }

    pub fn decode_permission_response(
        &self,
        result: Value,
        context: crate::PermissionResponseContext,
    ) -> Result<crate::ApplicationCommand, crate::ProtocolError> {
        match self.selected()? {
            NegotiatedProtocol::V1 => super::V1Adapter.decode_permission_response(result, context),
            #[cfg(feature = "acp-v2")]
            NegotiatedProtocol::V2 => super::V2Adapter::new(self._v2_runtime_enabled)
                .decode_permission_response(result, context),
        }
    }

    pub fn encode_session_result(
        &self,
        method: &str,
        result: crate::ApplicationResult,
    ) -> Result<Value, crate::ProtocolError> {
        match self.selected()? {
            NegotiatedProtocol::V1 => super::V1Adapter.encode_session_result(method, result),
            #[cfg(feature = "acp-v2")]
            NegotiatedProtocol::V2 => super::V2Adapter::new(self._v2_runtime_enabled)
                .encode_session_result(method, result),
        }
    }

    pub fn encode_result(
        &self,
        result: crate::ApplicationResult,
    ) -> Result<Value, crate::ProtocolError> {
        match self.selected()? {
            NegotiatedProtocol::V1 => super::V1Adapter.encode_result(result),
            #[cfg(feature = "acp-v2")]
            NegotiatedProtocol::V2 => {
                super::V2Adapter::new(self._v2_runtime_enabled).encode_result(result)
            }
        }
    }

    fn selected(&self) -> Result<NegotiatedProtocol, crate::ProtocolError> {
        self.negotiated_protocol()
            .ok_or(crate::ProtocolError::NotNegotiated)
    }

    fn application_context(&self) -> crate::ApplicationContext {
        crate::ApplicationContext {
            connection_principal: self.authenticated.principal.clone(),
            session_id: self
                .session_id
                .lock()
                .expect("session lock poisoned")
                .clone(),
        }
    }

    fn bind_session(&self, session_id: crate::SessionId) {
        *self.session_id.lock().expect("session lock poisoned") = Some(session_id);
    }

    fn clear_session(&self, session_id: &crate::SessionId) {
        let mut bound = self.session_id.lock().expect("session lock poisoned");
        if bound.as_ref() == Some(session_id) {
            *bound = None;
        }
    }
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
#[serde(deny_unknown_fields)]
pub struct NdjsonRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
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
    let connection = ProtocolConnection::new(connection.clone(), false);
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        let parsed = serde_json::from_str::<NdjsonRequest>(&line);
        let request_id = parsed
            .as_ref()
            .map(|request| request.id.clone())
            .unwrap_or(Value::Null);
        let response = match parsed {
            Ok(request) if request.jsonrpc != "2.0" => {
                Err("JSON-RPC version must be 2.0".to_owned())
            }
            Ok(request) if request.method == "initialize" => connection
                .initialize(request.params)
                .map_err(|error| error.to_string()),
            Ok(request) => dispatch_request(application, &connection, request).await,
            Err(error) => Err(format!("invalid JSON-RPC request: {error}")),
        };
        let payload = match response {
            Ok(value) => serde_json::json!({"jsonrpc": "2.0", "id": request_id, "result": value}),
            Err(error) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32000, "message": error}
            }),
        };
        writer
            .write_all(&serde_json::to_vec(&payload).expect("JSON value serializes"))
            .await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}

async fn dispatch_request(
    application: &crate::Application,
    connection: &ProtocolConnection,
    request: NdjsonRequest,
) -> Result<Value, String> {
    let is_session_method = request.method.starts_with("session/");
    let command = if is_session_method {
        connection.decode_session(&request.method, request.params)
    } else {
        connection.decode_extension(&request.method, request.params)
    }
    .map_err(|error| error.to_string())?;
    let closing_session = match &command {
        crate::ApplicationCommand::CloseSession(close) => Some(close.session_id.clone()),
        _ => None,
    };
    let result = application
        .dispatch(&connection.application_context(), command)
        .await
        .map_err(|error| error.to_string())?;
    if let crate::ApplicationResult::Session(session) = &result {
        connection.bind_session(session.view.id.clone());
    }
    if let Some(session_id) = closing_session {
        connection.clear_session(&session_id);
    }
    if is_session_method {
        connection.encode_session_result(&request.method, result)
    } else {
        connection.encode_result(result)
    }
    .map_err(|error| error.to_string())
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
    fn ndjson_request_is_a_standard_json_rpc_request_without_transport_semantics() {
        let request: super::NdjsonRequest = serde_json::from_value(serde_json::json!({
            "jsonrpc":"2.0", "id": 1, "method":"_kiteframe/run/get",
            "params":{"runId":"019ff3d8-9d86-7de0-9070-0f0185d5f413"}
        }))
        .unwrap();
        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(
            request.params["runId"],
            "019ff3d8-9d86-7de0-9070-0f0185d5f413"
        );
        assert!(
            serde_json::from_value::<super::NdjsonRequest>(serde_json::json!({
                "session_id":"019ff3d8-9d86-7de0-9070-0f0185d5f413", "method":"initialize",
                "payload":{}
            }))
            .is_err()
        );
    }
}
