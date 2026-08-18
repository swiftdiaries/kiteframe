use std::{
    collections::{HashMap, HashSet},
    error::Error as _,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use agent_client_protocol::{
    Agent, ByteStreams, Client, ConnectTo, ConnectionTo, Dispatch, HandleDispatchFrom, Handled,
    RawJsonRpcMessage, TransportBatchEntry, TransportFrame,
};
use agent_client_protocol_http::{AcpHttpServer, CorsOptions, ServerOptions};
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use http::{
    HeaderMap, HeaderValue, Method, StatusCode,
    header::{
        AUTHORIZATION, CONNECTION, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE,
        WWW_AUTHENTICATE,
    },
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tower::ServiceExt;

use super::{NegotiatedProtocol, ProtocolAdapter, ProtocolNegotiation};
use crate::{PrincipalId, Secret};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

    pub fn encode_extension_result(
        &self,
        method: &str,
        result: crate::ApplicationResult,
    ) -> Result<Value, crate::ProtocolError> {
        match self.selected()? {
            NegotiatedProtocol::V1 => super::V1Adapter.encode_extension_result(method, result),
            #[cfg(feature = "acp-v2")]
            NegotiatedProtocol::V2 => super::V2Adapter::new(self._v2_runtime_enabled)
                .encode_extension_result(method, result),
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

    fn bound_session(&self) -> Option<crate::SessionId> {
        self.session_id
            .lock()
            .expect("session lock poisoned")
            .clone()
    }
}

pub trait ConnectionAuthenticator: Send + Sync {
    fn authenticate(&self, headers: &HeaderMap) -> Result<PrincipalId, AuthError>;
}

#[derive(Clone)]
struct HashedBearerBinding {
    digest: [u8; 32],
    principal: PrincipalId,
}

#[derive(Clone)]
pub struct BearerAuthenticator {
    bindings: Vec<HashedBearerBinding>,
}

impl BearerAuthenticator {
    pub fn new(
        bindings: impl IntoIterator<Item = (Secret, PrincipalId)>,
    ) -> Result<Self, AuthError> {
        let mut digests = HashSet::new();
        let bindings = bindings
            .into_iter()
            .map(|(secret, principal)| {
                let digest: [u8; 32] = Sha256::digest(secret.expose().as_bytes()).into();
                if !digests.insert(digest) {
                    return Err(AuthError::DuplicateBinding);
                }
                Ok(HashedBearerBinding { digest, principal })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if bindings.is_empty() {
            return Err(AuthError::NoBindings);
        }
        Ok(Self { bindings })
    }
}

impl fmt::Debug for BearerAuthenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BearerAuthenticator")
            .field("binding_count", &self.bindings.len())
            .finish()
    }
}

impl ConnectionAuthenticator for BearerAuthenticator {
    fn authenticate(&self, headers: &HeaderMap) -> Result<PrincipalId, AuthError> {
        let value = headers
            .get(AUTHORIZATION)
            .ok_or(AuthError::MissingAuthorization)?
            .to_str()
            .map_err(|_| AuthError::InvalidAuthorization)?;
        let (_, token) = value
            .split_once(' ')
            .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.is_empty())
            .ok_or(AuthError::InvalidAuthorization)?;
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut principal = None;
        for binding in &self.bindings {
            if binding.digest.ct_eq(&candidate).unwrap_u8() == 1 {
                principal = Some(binding.principal.clone());
            }
        }
        principal.ok_or(AuthError::Unauthorized)
    }
}

#[derive(Clone, Debug)]
pub struct StaticBearerAuthenticator {
    inner: BearerAuthenticator,
}

impl StaticBearerAuthenticator {
    #[must_use]
    pub fn new(token: String, principal: PrincipalId) -> Self {
        Self {
            inner: BearerAuthenticator::new([(Secret::new(token), principal)])
                .expect("a single bearer binding is valid"),
        }
    }
}

impl ConnectionAuthenticator for StaticBearerAuthenticator {
    fn authenticate(&self, headers: &HeaderMap) -> Result<PrincipalId, AuthError> {
        self.inner.authenticate(headers)
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
    #[error("at least one bearer binding is required")]
    NoBindings,
    #[error("bearer secrets must be unique")]
    DuplicateBinding,
}

#[derive(Clone)]
pub struct AcpService {
    application: Arc<crate::Application>,
    store: Arc<crate::PostgresStore>,
    authenticated: AuthenticatedConnection,
    v2_runtime_enabled: bool,
    accepting: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    admission_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
}

impl AcpService {
    #[must_use]
    pub fn new(
        application: Arc<crate::Application>,
        store: Arc<crate::PostgresStore>,
        authenticated: AuthenticatedConnection,
        v2_runtime_enabled: bool,
    ) -> Self {
        Self {
            application,
            store,
            authenticated,
            v2_runtime_enabled,
            accepting: Arc::new(AtomicBool::new(true)),
            healthy: Arc::new(AtomicBool::new(true)),
            admission_permit: None,
        }
    }

    fn with_accepting(mut self, accepting: Arc<AtomicBool>) -> Self {
        self.accepting = accepting;
        self
    }

    fn with_health(mut self, healthy: Arc<AtomicBool>) -> Self {
        self.healthy = healthy;
        self
    }

    fn with_admission_permit(mut self, permit: Arc<tokio::sync::OwnedSemaphorePermit>) -> Self {
        self.admission_permit = Some(permit);
        self
    }

    pub fn begin_shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    fn v1_builder(self) -> impl ConnectTo<Client> {
        self.builder_with(Agent.builder())
    }

    #[cfg(feature = "acp-v2")]
    fn v2_builder(self) -> impl ConnectTo<Client> {
        self.builder_with(Agent.v2())
    }

    fn builder_with<H, R, C>(
        self,
        builder: agent_client_protocol::Builder<Agent, H, R, C>,
    ) -> impl ConnectTo<Client>
    where
        H: HandleDispatchFrom<Client> + 'static,
        R: agent_client_protocol::RunWithConnectionTo<Client> + 'static,
        C: agent_client_protocol::HandleConnectionClose<Client> + 'static,
    {
        let connection = Arc::new(ProtocolConnection::new(
            self.authenticated,
            self.v2_runtime_enabled,
        ));
        let permission_wake = Arc::new(tokio::sync::Notify::new());
        builder
            .with_handler(SemanticHandler {
                application: self.application.clone(),
                connection: connection.clone(),
                accepting: self.accepting.clone(),
                healthy: self.healthy.clone(),
                permission_wake: permission_wake.clone(),
                _admission_permit: self.admission_permit.clone(),
            })
            .with_spawned(move |client| {
                observe_permissions(
                    PermissionObserver {
                        application: self.application,
                        store: self.store,
                        connection,
                        wake: permission_wake,
                        accepting: self.accepting,
                        healthy: self.healthy,
                    },
                    client,
                )
            })
    }
}

impl ConnectTo<Client> for AcpService {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> agent_client_protocol::Result<()> {
        let accepting = self.accepting.clone();
        let connecting = async move {
            #[cfg(feature = "acp-v2")]
            {
                let v2_runtime_enabled = self.v2_runtime_enabled;
                let mut router = Agent.protocol_router().with_v1(self.clone().v1_builder());
                if v2_runtime_enabled {
                    router = router.with_v2(self.v2_builder());
                }
                router.connect_to(client).await
            }
            #[cfg(not(feature = "acp-v2"))]
            {
                self.v1_builder().connect_to(client).await
            }
        };
        tokio::pin!(connecting);
        tokio::select! {
            result = &mut connecting => result,
            () = wait_until_draining(accepting) => Ok(()),
        }
    }
}

async fn wait_until_draining(accepting: Arc<AtomicBool>) {
    while accepting.load(Ordering::Acquire) {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn connection_accepting(accepting: &AtomicBool, healthy: &AtomicBool) -> bool {
    accepting.load(Ordering::Acquire) && healthy.load(Ordering::Acquire)
}

pub async fn serve_acp_stream<R, W>(
    reader: R,
    writer: W,
    application: Arc<crate::Application>,
    store: Arc<crate::PostgresStore>,
    connection: AuthenticatedConnection,
    v2_runtime_enabled: bool,
) -> agent_client_protocol::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    AcpService::new(application, store, connection, v2_runtime_enabled)
        .connect_to(ByteStreams::new(writer.compat_write(), reader.compat()))
        .await
}

#[derive(Clone)]
struct SemanticHandler {
    application: Arc<crate::Application>,
    connection: Arc<ProtocolConnection>,
    accepting: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    permission_wake: Arc<tokio::sync::Notify>,
    _admission_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
}

const PERMISSION_ALLOW_ONCE: &str = "allow-once";
const PERMISSION_ALLOW_SESSION: &str = "allow-session";
const PERMISSION_DENY_ONCE: &str = "deny-once";
const PERMISSION_DENY_SESSION: &str = "deny-session";

struct PermissionObserver {
    application: Arc<crate::Application>,
    store: Arc<crate::PostgresStore>,
    connection: Arc<ProtocolConnection>,
    wake: Arc<tokio::sync::Notify>,
    accepting: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
}

const MAX_INFLIGHT_PERMISSIONS: usize = 32;

async fn observe_permissions(
    observer: PermissionObserver,
    client: ConnectionTo<Client>,
) -> agent_client_protocol::Result<()> {
    let PermissionObserver {
        application,
        store,
        connection,
        wake: permission_wake,
        accepting,
        healthy,
    } = observer;
    let mut last_offered = HashMap::<crate::PermissionRequestId, std::time::Instant>::new();
    let mut discovery_session = None;
    let inflight = Arc::new(Mutex::new(HashMap::<
        crate::PermissionRequestId,
        InflightPermission,
    >::new()));
    loop {
        if client.is_incoming_closed() || !accepting.load(Ordering::Acquire) {
            cancel_inflight_permissions(&inflight);
            return Ok(());
        }
        if !healthy.load(Ordering::Acquire) {
            wait_for_permission_wake(&permission_wake).await;
            continue;
        }
        let Some(session_id) = connection.bound_session() else {
            cancel_inflight_permissions(&inflight);
            last_offered.clear();
            wait_for_permission_wake(&permission_wake).await;
            continue;
        };
        if discovery_session.as_ref() != Some(&session_id) {
            discovery_session = Some(session_id.clone());
            last_offered.clear();
        }
        cancel_resolved_permissions(&store, &inflight).await?;
        last_offered.retain(|_, instant| instant.elapsed() < std::time::Duration::from_secs(1));
        let inflight_count = inflight
            .lock()
            .expect("permission inflight lock poisoned")
            .len();
        if inflight_count >= MAX_INFLIGHT_PERMISSIONS {
            wait_for_permission_wake(&permission_wake).await;
            continue;
        }
        while last_offered.len() > MAX_INFLIGHT_PERMISSIONS {
            let Some(oldest) = last_offered
                .iter()
                .min_by_key(|(_, instant)| **instant)
                .map(|(request_id, _)| request_id.clone())
            else {
                break;
            };
            last_offered.remove(&oldest);
        }
        let excluded = inflight
            .lock()
            .expect("permission inflight lock poisoned")
            .keys()
            .cloned()
            .chain(last_offered.keys().cloned())
            .collect::<HashSet<_>>();
        let available = MAX_INFLIGHT_PERMISSIONS - inflight_count;
        let pending = unresolved_permissions(&store, &session_id, &excluded, available).await?;
        for delivery in pending {
            let request_id = delivery.request_id.clone();
            if inflight
                .lock()
                .expect("permission inflight lock poisoned")
                .contains_key(&request_id)
                || last_offered
                    .get(&request_id)
                    .is_some_and(|instant| instant.elapsed() < std::time::Duration::from_secs(1))
            {
                continue;
            }
            if !accepting.load(Ordering::Acquire) {
                return Ok(());
            }
            if !healthy.load(Ordering::Acquire) {
                break;
            }
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            inflight
                .lock()
                .expect("permission inflight lock poisoned")
                .insert(request_id.clone(), InflightPermission { cancel: cancel_tx });
            last_offered.insert(request_id.clone(), std::time::Instant::now());
            if let Err(error) = send_permission_request(
                PermissionRuntime {
                    application: application.clone(),
                    connection: connection.clone(),
                    client: client.clone(),
                    inflight: inflight.clone(),
                    accepting: accepting.clone(),
                    healthy: healthy.clone(),
                    cancel: cancel_rx,
                    wake: permission_wake.clone(),
                },
                session_id.clone(),
                delivery,
            ) {
                inflight
                    .lock()
                    .expect("permission inflight lock poisoned")
                    .remove(&request_id);
                return Err(error);
            }
        }
        wait_for_permission_wake(&permission_wake).await;
    }
}

struct InflightPermission {
    cancel: tokio::sync::oneshot::Sender<()>,
}

type InflightPermissions = Arc<Mutex<HashMap<crate::PermissionRequestId, InflightPermission>>>;

async fn cancel_resolved_permissions(
    store: &crate::PostgresStore,
    inflight: &InflightPermissions,
) -> agent_client_protocol::Result<()> {
    let request_ids = inflight
        .lock()
        .expect("permission inflight lock poisoned")
        .keys()
        .map(|request_id| *request_id.as_uuid())
        .collect::<Vec<_>>();
    if request_ids.is_empty() {
        return Ok(());
    }
    let pending = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM permission_requests WHERE id = ANY($1) AND state = 'pending'",
    )
    .bind(&request_ids)
    .fetch_all(store.pool())
    .await
    .map_err(|_| agent_client_protocol::Error::internal_error())?
    .into_iter()
    .map(crate::PermissionRequestId::new)
    .collect::<HashSet<_>>();
    let cancellations = {
        let mut inflight = inflight.lock().expect("permission inflight lock poisoned");
        let resolved = inflight
            .keys()
            .filter(|request_id| !pending.contains(*request_id))
            .cloned()
            .collect::<Vec<_>>();
        resolved
            .into_iter()
            .filter_map(|request_id| inflight.remove(&request_id))
            .collect::<Vec<_>>()
    };
    for cancel in cancellations {
        let _ = cancel.cancel.send(());
    }
    Ok(())
}

fn cancel_inflight_permissions(inflight: &InflightPermissions) {
    let cancellations = {
        let mut inflight = inflight.lock().expect("permission inflight lock poisoned");
        std::mem::take(&mut *inflight)
            .into_values()
            .collect::<Vec<_>>()
    };
    for cancel in cancellations {
        let _ = cancel.cancel.send(());
    }
}

async fn wait_for_permission_wake(wake: &tokio::sync::Notify) {
    tokio::select! {
        () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        () = wake.notified() => {}
    }
}

struct PermissionDelivery {
    request_id: crate::PermissionRequestId,
    run_id: crate::RunId,
    pending: crate::PendingPermission,
}

async fn unresolved_permissions(
    store: &crate::PostgresStore,
    session_id: &crate::SessionId,
    excluded: &HashSet<crate::PermissionRequestId>,
    limit: usize,
) -> agent_client_protocol::Result<Vec<PermissionDelivery>> {
    let excluded = excluded
        .iter()
        .map(|request_id| *request_id.as_uuid())
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        "WITH claimed AS ( \
           SELECT p.id FROM permission_requests p \
           JOIN runs r ON r.id = p.run_id \
           WHERE r.session_id = $1 AND p.state = 'pending' \
             AND NOT (p.id = ANY($2)) \
           ORDER BY COALESCE(p.last_offered_at, p.created_at), p.created_at, p.id \
           LIMIT $3 FOR UPDATE OF p SKIP LOCKED \
         ), offered AS ( \
           UPDATE permission_requests p SET last_offered_at = CURRENT_TIMESTAMP \
           FROM claimed WHERE p.id = claimed.id RETURNING p.* \
         ) \
         SELECT p.id AS request_id, p.run_id, f.id AS frame_id, f.revision, \
         t.id AS tool_call_id, p.provider_call_id, p.qualified_tool, \
         p.descriptor_digest, t.arguments \
         FROM offered p \
         JOIN frames f ON f.id = p.frame_id \
         JOIN tool_calls t ON t.frame_id = p.frame_id \
            AND t.provider_call_id = p.provider_call_id \
         ORDER BY p.last_offered_at, p.created_at, p.id",
    )
    .bind(*session_id.as_uuid())
    .bind(excluded)
    .bind(i64::try_from(limit.min(MAX_INFLIGHT_PERMISSIONS)).unwrap_or(1))
    .fetch_all(store.pool())
    .await
    .map_err(|_| agent_client_protocol::Error::internal_error())?;
    Ok(rows
        .into_iter()
        .map(|row| PermissionDelivery {
            request_id: crate::PermissionRequestId::new(row.get("request_id")),
            run_id: crate::RunId::new(row.get("run_id")),
            pending: crate::PendingPermission {
                tool_call_id: row.get("tool_call_id"),
                frame_id: crate::FrameId::new(row.get("frame_id")),
                revision: row.get::<i64, _>("revision") as u64,
                provider_call_id: row.get("provider_call_id"),
                qualified_name: row.get("qualified_tool"),
                descriptor_digest: row.get("descriptor_digest"),
                arguments: row.get("arguments"),
                resolved: false,
                resolution_idempotency_key: None,
            },
        })
        .collect())
}

fn permission_context(
    session_id: crate::SessionId,
    run_id: crate::RunId,
    request_id: crate::PermissionRequestId,
) -> Result<crate::PermissionResponseContext, crate::ProtocolError> {
    Ok(crate::PermissionResponseContext::new(
        session_id,
        run_id,
        request_id.clone(),
        crate::IdempotencyKey::new(format!("acp-permission-{}", request_id.as_uuid()))
            .map_err(|error| crate::ProtocolError::InvalidPayload(error.to_string()))?,
    )
    .with_option(PERMISSION_ALLOW_ONCE, crate::PermissionChoice::AllowOnce)
    .with_option(
        PERMISSION_ALLOW_SESSION,
        crate::PermissionChoice::AllowToolForSession,
    )
    .with_option(PERMISSION_DENY_ONCE, crate::PermissionChoice::DenyOnce)
    .with_option(
        PERMISSION_DENY_SESSION,
        crate::PermissionChoice::DenyToolForSession,
    ))
}

struct PermissionRuntime {
    application: Arc<crate::Application>,
    connection: Arc<ProtocolConnection>,
    client: ConnectionTo<Client>,
    inflight: InflightPermissions,
    accepting: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    cancel: tokio::sync::oneshot::Receiver<()>,
    wake: Arc<tokio::sync::Notify>,
}

struct PermissionResolutionRuntime {
    application: Arc<crate::Application>,
    connection: Arc<ProtocolConnection>,
    inflight: InflightPermissions,
    accepting: Arc<AtomicBool>,
    healthy: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
}

fn send_permission_request(
    runtime: PermissionRuntime,
    session_id: crate::SessionId,
    delivery: PermissionDelivery,
) -> agent_client_protocol::Result<()> {
    let PermissionDelivery {
        request_id,
        run_id,
        pending,
        ..
    } = delivery;
    let PermissionRuntime {
        application,
        connection,
        client,
        inflight,
        accepting,
        healthy,
        cancel,
        wake,
    } = runtime;
    let context = permission_context(session_id.clone(), run_id, request_id.clone())
        .map_err(protocol_sdk_error)?;
    match connection.selected().map_err(protocol_sdk_error)? {
        NegotiatedProtocol::V1 => {
            use agent_client_protocol::schema::v1 as acp;
            let request = acp::RequestPermissionRequest::new(
                acp::SessionId::new(session_id.as_uuid().to_string()),
                acp::ToolCall::new(pending.provider_call_id, pending.qualified_name)
                    .status(acp::ToolCallStatus::Pending)
                    .raw_input(pending.arguments)
                    .into(),
                vec![
                    acp::PermissionOption::new(
                        PERMISSION_ALLOW_ONCE,
                        "Allow once",
                        acp::PermissionOptionKind::AllowOnce,
                    ),
                    acp::PermissionOption::new(
                        PERMISSION_ALLOW_SESSION,
                        "Allow for session",
                        acp::PermissionOptionKind::AllowAlways,
                    ),
                    acp::PermissionOption::new(
                        PERMISSION_DENY_ONCE,
                        "Deny once",
                        acp::PermissionOptionKind::RejectOnce,
                    ),
                    acp::PermissionOption::new(
                        PERMISSION_DENY_SESSION,
                        "Deny for session",
                        acp::PermissionOptionKind::RejectAlways,
                    ),
                ],
            );
            let sent = client.send_request(request);
            let cancelled_request_id = request_id.clone();
            client.spawn(async move {
                tokio::select! {
                    result = sent.block_task() => {
                        resolve_permission_response(
                            PermissionResolutionRuntime {
                                application,
                                connection,
                                inflight,
                                accepting,
                                healthy,
                                wake,
                            },
                            result,
                            context,
                        )
                        .await
                    }
                    _ = cancel => {
                        inflight
                            .lock()
                            .expect("permission inflight lock poisoned")
                            .remove(&cancelled_request_id);
                        wake.notify_one();
                        Ok(())
                    }
                }
            })?;
        }
        #[cfg(feature = "acp-v2")]
        NegotiatedProtocol::V2 => {
            use agent_client_protocol::schema::v2 as acp;
            let request = acp::RequestPermissionRequest::new(
                acp::SessionId::new(session_id.as_uuid().to_string()),
                format!("Allow {}?", pending.qualified_name),
                vec![
                    acp::PermissionOption::new(
                        PERMISSION_ALLOW_ONCE,
                        "Allow once",
                        acp::PermissionOptionKind::AllowOnce,
                    ),
                    acp::PermissionOption::new(
                        PERMISSION_ALLOW_SESSION,
                        "Allow for session",
                        acp::PermissionOptionKind::AllowAlways,
                    ),
                    acp::PermissionOption::new(
                        PERMISSION_DENY_ONCE,
                        "Deny once",
                        acp::PermissionOptionKind::RejectOnce,
                    ),
                    acp::PermissionOption::new(
                        PERMISSION_DENY_SESSION,
                        "Deny for session",
                        acp::PermissionOptionKind::RejectAlways,
                    ),
                ],
            )
            .subject(acp::RequestPermissionSubject::from(
                acp::ToolCallUpdate::new(pending.provider_call_id)
                    .title(pending.qualified_name)
                    .status(acp::ToolCallStatus::Pending)
                    .raw_input(pending.arguments),
            ));
            let sent = client.send_request(request);
            let cancelled_request_id = request_id.clone();
            client.spawn(async move {
                tokio::select! {
                    result = sent.block_task() => {
                        resolve_permission_response(
                            PermissionResolutionRuntime {
                                application,
                                connection,
                                inflight,
                                accepting,
                                healthy,
                                wake,
                            },
                            result,
                            context,
                        )
                        .await
                    }
                    _ = cancel => {
                        inflight
                            .lock()
                            .expect("permission inflight lock poisoned")
                            .remove(&cancelled_request_id);
                        wake.notify_one();
                        Ok(())
                    }
                }
            })?;
        }
    }
    Ok(())
}

async fn resolve_permission_response<R: serde::Serialize>(
    runtime: PermissionResolutionRuntime,
    result: agent_client_protocol::Result<R>,
    context: crate::PermissionResponseContext,
) -> agent_client_protocol::Result<()> {
    let PermissionResolutionRuntime {
        application,
        connection,
        inflight,
        accepting,
        healthy,
        wake,
    } = runtime;
    inflight
        .lock()
        .expect("permission inflight lock poisoned")
        .remove(&context.permission_request_id);
    wake.notify_one();
    let result = match result {
        Ok(result) => serde_json::to_value(result).map_err(|error| {
            agent_client_protocol::Error::internal_error().data(error.to_string())
        })?,
        Err(error) => {
            tracing::warn!(code = ?error.code, "ACP permission request failed");
            return Ok(());
        }
    };
    let response_session = context.session_id.clone();
    let command = match connection.decode_permission_response(result, context) {
        Ok(command) => command,
        Err(_) => {
            tracing::warn!("ACP permission response was rejected");
            return Ok(());
        }
    };
    if !connection_accepting(&accepting, &healthy) {
        return Ok(());
    }
    if application
        .dispatch(
            &crate::ApplicationContext {
                connection_principal: connection.authenticated.principal.clone(),
                session_id: Some(response_session),
            },
            command,
        )
        .await
        .is_err()
    {
        tracing::warn!("ACP permission response application failed");
    }
    Ok(())
}

impl HandleDispatchFrom<Client> for SemanticHandler {
    async fn handle_dispatch_from(
        &mut self,
        dispatch: Dispatch,
        _client: ConnectionTo<Client>,
    ) -> agent_client_protocol::Result<Handled<Dispatch>> {
        match dispatch {
            Dispatch::Request(request, responder) => {
                let method = request.method.clone();
                let task_revision = logged_task_revision(&method, &request.params);
                let (result, error_code) = if connection_accepting(&self.accepting, &self.healthy) {
                    let dispatched = dispatch_application(
                        &self.application,
                        &self.connection,
                        method.as_str(),
                        request.params,
                    )
                    .await;
                    let error_code = dispatched
                        .as_ref()
                        .err()
                        .map_or("none", logged_protocol_error);
                    (dispatched.map_err(protocol_sdk_error), error_code)
                } else {
                    (
                        Err(agent_client_protocol::Error::request_cancelled()
                            .data("service is shutting down")),
                        "request_cancelled",
                    )
                };
                let succeeded = result.is_ok();
                tracing::info!(
                    method = logged_method(&method),
                    transport = ?self.connection.authenticated.transport,
                    success = result.is_ok(),
                    error_code,
                    task_revision = task_revision.unwrap_or_default(),
                    task_revision_known = task_revision.is_some(),
                    "ACP request handled"
                );
                responder.respond_with_result(result.map(|outcome| outcome.value))?;
                if succeeded {
                    self.permission_wake.notify_one();
                }
                Ok(Handled::Yes)
            }
            Dispatch::Notification(notification) => {
                let task_revision =
                    logged_task_revision(&notification.method, &notification.params);
                let error_code = if connection_accepting(&self.accepting, &self.healthy) {
                    dispatch_application(
                        &self.application,
                        &self.connection,
                        notification.method.as_str(),
                        notification.params,
                    )
                    .await
                    .as_ref()
                    .err()
                    .map_or("none", logged_protocol_error)
                } else {
                    "request_cancelled"
                };
                tracing::info!(
                    method = logged_method(&notification.method),
                    transport = ?self.connection.authenticated.transport,
                    error_code,
                    task_revision = task_revision.unwrap_or_default(),
                    task_revision_known = task_revision.is_some(),
                    "ACP notification handled"
                );
                Ok(Handled::Yes)
            }
            Dispatch::Response(_, _) => Ok(Handled::No {
                message: dispatch,
                retry: false,
            }),
        }
    }

    fn describe_chain(&self) -> impl fmt::Debug {
        "kiteframe semantic dispatcher"
    }
}

fn logged_task_revision(method: &str, params: &Value) -> Option<u64> {
    match method {
        "_kiteframe/agent/start"
        | "_kiteframe/task/pause"
        | "_kiteframe/task/resume"
        | "_kiteframe/task/cancel" => params.get("expectedTaskRevision")?.as_u64(),
        _ => None,
    }
}

fn logged_protocol_error(error: &crate::ProtocolError) -> &'static str {
    match error {
        crate::ProtocolError::StableApplication { code, .. } => code,
        crate::ProtocolError::InvalidPayload(_)
        | crate::ProtocolError::UnsupportedVersion(_)
        | crate::ProtocolError::V2Disabled
        | crate::ProtocolError::McpUnavailable => "invalid_params",
        crate::ProtocolError::UnsupportedMethod(_) => "method_not_found",
        crate::ProtocolError::MissingSession => "missing_session",
        crate::ProtocolError::SessionMismatch => "session_mismatch",
        crate::ProtocolError::AlreadyNegotiated => "already_negotiated",
        crate::ProtocolError::NotNegotiated => "not_negotiated",
        crate::ProtocolError::PermissionCancelled => "permission_cancelled",
        crate::ProtocolError::SessionExpired => "session_expired",
        crate::ProtocolError::WorkingDirectoryMismatch => "working_directory_mismatch",
        crate::ProtocolError::McpConfigurationRequired => "mcp_configuration_required",
        crate::ProtocolError::Application(_) | crate::ProtocolError::UnexpectedResult(_) => {
            "application_failure"
        }
    }
}

fn logged_method(method: &str) -> &'static str {
    match method {
        "initialize" => "initialize",
        "session/new" => "session/new",
        "session/load" => "session/load",
        "session/list" => "session/list",
        "session/resume" => "session/resume",
        "session/close" => "session/close",
        "_kiteframe/workflow/publish" => "_kiteframe/workflow/publish",
        "_kiteframe/definition/publish" => "_kiteframe/definition/publish",
        "_kiteframe/run/start" => "_kiteframe/run/start",
        "_kiteframe/run/get" => "_kiteframe/run/get",
        "_kiteframe/run/answer" => "_kiteframe/run/answer",
        "_kiteframe/run/cancel" => "_kiteframe/run/cancel",
        "_kiteframe/run/recovery/retry" => "_kiteframe/run/recovery/retry",
        "_kiteframe/session/policy/update" => "_kiteframe/session/policy/update",
        "_kiteframe/run/event" => "_kiteframe/run/event",
        "_kiteframe/task/submit" => "_kiteframe/task/submit",
        "_kiteframe/task/get" => "_kiteframe/task/get",
        "_kiteframe/agent/start" => "_kiteframe/agent/start",
        "_kiteframe/agent/get" => "_kiteframe/agent/get",
        "_kiteframe/task/pause" => "_kiteframe/task/pause",
        "_kiteframe/task/resume" => "_kiteframe/task/resume",
        "_kiteframe/task/cancel" => "_kiteframe/task/cancel",
        "_kiteframe/task/events" => "_kiteframe/task/events",
        _ => "unknown",
    }
}

fn protocol_sdk_error(error: crate::ProtocolError) -> agent_client_protocol::Error {
    super::types::encode_protocol_error(&error)
}

#[derive(Clone)]
pub struct RemoteService {
    state: Arc<RemoteState>,
}

struct RemoteState {
    application: Arc<crate::Application>,
    store: Arc<crate::PostgresStore>,
    authenticator: Arc<dyn ConnectionAuthenticator>,
    ready: Arc<AtomicBool>,
    model_ready: Arc<AtomicBool>,
    v2_runtime_enabled: bool,
    routers: tokio::sync::RwLock<HashMap<RouterKey, Router>>,
    admission: Arc<tokio::sync::Semaphore>,
    admission_gate: Arc<tokio::sync::Mutex<()>>,
}

struct AdmissionReservation {
    state: Mutex<AdmissionReservationState>,
}

// A request-local reservation is the only pre-factory owner. Its state becomes
// unclaimable before either admission resource is released.
enum AdmissionReservationState {
    Reserved {
        permit: tokio::sync::OwnedSemaphorePermit,
        gate: tokio::sync::OwnedMutexGuard<()>,
    },
    Claimed,
    Cancelled,
}

impl AdmissionReservation {
    fn new(
        permit: tokio::sync::OwnedSemaphorePermit,
        gate: tokio::sync::OwnedMutexGuard<()>,
    ) -> Self {
        Self {
            state: Mutex::new(AdmissionReservationState::Reserved { permit, gate }),
        }
    }

    fn claim(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.claim_before_gate_release(|| {})
    }

    fn claim_before_gate_release(
        &self,
        before_gate_release: impl FnOnce(),
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let mut state = self
            .state
            .lock()
            .expect("ACP admission reservation poisoned");
        if !matches!(*state, AdmissionReservationState::Reserved { .. }) {
            return None;
        }
        let AdmissionReservationState::Reserved { permit, gate } =
            std::mem::replace(&mut *state, AdmissionReservationState::Claimed)
        else {
            unreachable!("reservation state checked above");
        };
        drop(state);
        before_gate_release();
        drop(gate);
        Some(permit)
    }

    fn cancel(&self) {
        self.cancel_before_gate_release(|| {});
    }

    fn cancel_before_gate_release(&self, before_gate_release: impl FnOnce()) {
        let mut state = self
            .state
            .lock()
            .expect("ACP admission reservation poisoned");
        if !matches!(*state, AdmissionReservationState::Reserved { .. }) {
            return;
        }
        let AdmissionReservationState::Reserved { permit, gate } =
            std::mem::replace(&mut *state, AdmissionReservationState::Cancelled)
        else {
            unreachable!("reservation state checked above");
        };
        drop(state);
        drop(permit);
        before_gate_release();
        drop(gate);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RouterKey {
    principal: PrincipalId,
    transport: TransportKind,
}

impl RemoteService {
    #[must_use]
    pub fn new(
        application: Arc<crate::Application>,
        authenticator: Arc<dyn ConnectionAuthenticator>,
        store: Arc<crate::PostgresStore>,
        model_ready: Arc<AtomicBool>,
    ) -> Self {
        Self::with_v2_and_limit(application, authenticator, store, model_ready, false, 64)
    }

    #[must_use]
    pub fn with_v2(
        application: Arc<crate::Application>,
        authenticator: Arc<dyn ConnectionAuthenticator>,
        store: Arc<crate::PostgresStore>,
        model_ready: Arc<AtomicBool>,
        v2_runtime_enabled: bool,
    ) -> Self {
        Self::with_v2_and_limit(
            application,
            authenticator,
            store,
            model_ready,
            v2_runtime_enabled,
            64,
        )
    }

    #[must_use]
    pub fn with_v2_and_limit(
        application: Arc<crate::Application>,
        authenticator: Arc<dyn ConnectionAuthenticator>,
        store: Arc<crate::PostgresStore>,
        model_ready: Arc<AtomicBool>,
        v2_runtime_enabled: bool,
        max_connections: usize,
    ) -> Self {
        Self {
            state: Arc::new(RemoteState {
                application,
                store,
                authenticator,
                ready: Arc::new(AtomicBool::new(false)),
                model_ready,
                v2_runtime_enabled,
                routers: tokio::sync::RwLock::new(HashMap::new()),
                admission: Arc::new(tokio::sync::Semaphore::new(max_connections.max(1))),
                admission_gate: Arc::new(tokio::sync::Mutex::new(())),
            }),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/acp", any(handle_acp))
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .with_state(self.state.clone())
    }

    pub fn mark_ready(&self) {
        self.state.ready.store(true, Ordering::Release);
    }

    pub fn begin_shutdown(&self) {
        self.state.ready.store(false, Ordering::Release);
    }

    #[must_use]
    pub fn available_connection_capacity(&self) -> usize {
        self.state.admission.available_permits()
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<Arc<RemoteState>>) -> Response {
    if !state.ready.load(Ordering::Acquire) || !state.model_ready.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if !database_healthy(&state).await {
        return retryable_response(StatusCode::SERVICE_UNAVAILABLE);
    }
    StatusCode::OK.into_response()
}

async fn handle_acp(State(state): State<Arc<RemoteState>>, mut request: Request<Body>) -> Response {
    let principal = match state.authenticator.authenticate(request.headers()) {
        Ok(principal) => principal,
        Err(error) => return auth_response(error),
    };
    request.headers_mut().remove(AUTHORIZATION);
    if !state.ready.load(Ordering::Acquire) || !state.model_ready.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if !database_healthy(&state).await {
        return retryable_response(StatusCode::SERVICE_UNAVAILABLE);
    }
    let transport = match classify_transport(request.method(), request.headers()) {
        Ok(transport) => transport,
        Err(error) => return error.into_response(),
    };
    let key = RouterKey {
        principal: principal.clone(),
        transport,
    };
    let creates_connection = if transport == TransportKind::WebSocket {
        true
    } else {
        match streamable_http_creates_connection(&mut request).await {
            Ok(creates_connection) => creates_connection,
            Err(response) => return response,
        }
    };
    let mut admission_guard = if creates_connection {
        Some(state.admission_gate.clone().lock_owned().await)
    } else {
        None
    };
    if creates_connection && state.admission.available_permits() == 0 {
        return retryable_response(StatusCode::TOO_MANY_REQUESTS);
    }
    let authenticated = AuthenticatedConnection {
        principal,
        transport,
    };
    let (router, mut cleanup) = if transport == TransportKind::WebSocket {
        let permit = match state.admission.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return retryable_response(StatusCode::TOO_MANY_REQUESTS),
        };
        // The exact factory token and its cleanup guard are armed together,
        // before dispatch introduces the next cancellation point.
        let (reservation, cleanup) = AdmissionCleanup::new(
            permit,
            admission_guard
                .take()
                .expect("WebSocket admission owns the gate"),
        );
        (
            acp_connection_router(state.clone(), authenticated, Some(reservation)),
            Some(cleanup),
        )
    } else {
        let router = if let Some(router) = state.routers.read().await.get(&key).cloned() {
            router
        } else {
            let mut routers = state.routers.write().await;
            routers
                .entry(key)
                .or_insert_with(|| acp_connection_router(state.clone(), authenticated, None))
                .clone()
        };
        (router, None)
    };
    let response = router
        .oneshot(request)
        .await
        .expect("the ACP SDK router is infallible");
    if response.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(cleanup) = cleanup.take()
    {
        cleanup.reap_after(std::time::Duration::from_secs(1));
    }
    response
}

const ACP_HTTP_MAX_POST_BODY_BYTES: usize = 16 * 1024 * 1024;

async fn streamable_http_creates_connection(request: &mut Request<Body>) -> Result<bool, Response> {
    if request.method() != Method::POST
        || !request
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|content_type| content_type.starts_with("application/json"))
        || request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > ACP_HTTP_MAX_POST_BODY_BYTES)
    {
        return Ok(false);
    }

    let body = std::mem::replace(request.body_mut(), Body::empty());
    let body = match axum::body::to_bytes(body, ACP_HTTP_MAX_POST_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            return Err(if body_limit_exceeded(&error) {
                (StatusCode::PAYLOAD_TOO_LARGE, "POST body too large").into_response()
            } else {
                StatusCode::BAD_REQUEST.into_response()
            });
        }
    };
    *request.body_mut() = Body::from(body.clone());
    let Ok(body) = std::str::from_utf8(&body) else {
        return Ok(false);
    };
    let session_id = request
        .headers()
        .get("acp-session-id")
        .and_then(|value| value.to_str().ok());
    Ok(frame_starts_connection(
        &TransportFrame::parse_json(body),
        session_id,
    ))
}

fn body_limit_exceeded(error: &axum::Error) -> bool {
    let mut source = error.source();
    while let Some(error) = source {
        if error.to_string() == "length limit exceeded" {
            return true;
        }
        source = error.source();
    }
    false
}

fn frame_starts_connection(frame: &TransportFrame, session_id: Option<&str>) -> bool {
    fn is_initialize(message: &RawJsonRpcMessage) -> bool {
        matches!(
            message,
            RawJsonRpcMessage::Request(request) if request.method.as_ref() == "initialize"
        )
    }

    match frame {
        TransportFrame::Single(message) => is_initialize(message),
        TransportFrame::Malformed { .. } => false,
        TransportFrame::Batch(batch) => {
            let initialize_index =
                batch
                    .entries()
                    .enumerate()
                    .find_map(|(index, entry)| match entry {
                        TransportBatchEntry::Message(RawJsonRpcMessage::Response(_)) => None,
                        TransportBatchEntry::Message(message) if is_initialize(message) => {
                            Some(index)
                        }
                        TransportBatchEntry::Message(_) => Some(usize::MAX),
                        TransportBatchEntry::Malformed { raw, .. } if response_only_shape(raw) => {
                            None
                        }
                        TransportBatchEntry::Malformed { .. } => Some(usize::MAX),
                    });
            let Some(initialize_index) = initialize_index.filter(|index| *index != usize::MAX)
            else {
                return false;
            };
            for (index, entry) in batch.entries().enumerate() {
                if index == initialize_index {
                    continue;
                }
                match entry {
                    TransportBatchEntry::Message(message)
                        if !message_route_is_valid(message, session_id) =>
                    {
                        return false;
                    }
                    TransportBatchEntry::Message(_) | TransportBatchEntry::Malformed { .. } => {}
                }
            }
            true
        }
    }
}

fn response_only_shape(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        !object.contains_key("method")
            && (object.contains_key("result") || object.contains_key("error"))
    })
}

fn message_route_is_valid(message: &RawJsonRpcMessage, session_id: Option<&str>) -> bool {
    let Some(method) = message_method(message) else {
        return true;
    };
    let connection_scoped = connection_scoped_message(message, method);
    if let Some(session_id) = session_id
        && !connection_scoped
    {
        let params = match message {
            RawJsonRpcMessage::Request(request) => request.params.as_ref(),
            RawJsonRpcMessage::Notification(notification) => notification.params.as_ref(),
            RawJsonRpcMessage::Response(_) => None,
        };
        return match params {
            Some(agent_client_protocol::RawJsonRpcParams::Object(params)) => {
                params.get("sessionId").is_none_or(|existing| {
                    existing
                        .as_str()
                        .is_some_and(|existing| existing == session_id)
                })
            }
            Some(agent_client_protocol::RawJsonRpcParams::Array(_)) => false,
            None => true,
        };
    }

    let routed_session = if connection_scoped {
        None
    } else {
        match message {
            RawJsonRpcMessage::Request(request) => request.params.as_ref(),
            RawJsonRpcMessage::Notification(notification) => notification.params.as_ref(),
            RawJsonRpcMessage::Response(_) => None,
        }
        .and_then(|params| match params {
            agent_client_protocol::RawJsonRpcParams::Object(params) => params
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            agent_client_protocol::RawJsonRpcParams::Array(_) => None,
        })
    };
    !method_requires_session_header(method) || routed_session.is_some()
}

fn message_method(message: &RawJsonRpcMessage) -> Option<&str> {
    match message {
        RawJsonRpcMessage::Request(request) => Some(request.method.as_ref()),
        RawJsonRpcMessage::Notification(notification) => Some(notification.method.as_ref()),
        RawJsonRpcMessage::Response(_) => None,
    }
}

fn connection_scoped_message(message: &RawJsonRpcMessage, method: &str) -> bool {
    if method.starts_with("$/") {
        return true;
    }
    let RawJsonRpcMessage::Notification(notification) = message else {
        return false;
    };
    let params = notification.params.clone().map_or(
        Value::Null,
        agent_client_protocol::RawJsonRpcParams::into_value,
    );
    agent_client_protocol::UntypedMessage::new(notification.method.as_ref(), params).is_ok_and(
        |notification| agent_client_protocol::is_cancel_request_notification(&notification),
    )
}

fn method_requires_session_header(method: &str) -> bool {
    matches!(
        method,
        "session/prompt"
            | "session/cancel"
            | "session/close"
            | "session/delete"
            | "session/fork"
            | "session/load"
            | "session/resume"
            | "session/set_config_option"
            | "session/set_mode"
            | "session/set_model"
    )
}

fn acp_connection_router(
    state: Arc<RemoteState>,
    authenticated: AuthenticatedConnection,
    reservation: Option<Arc<AdmissionReservation>>,
) -> Router {
    let application = state.application.clone();
    let store = state.store.clone();
    let v2_runtime_enabled = state.v2_runtime_enabled;
    let accepting = state.ready.clone();
    let healthy = state.model_ready.clone();
    AcpHttpServer::new(move || {
        let permit = match &reservation {
            Some(reservation) => reservation.claim(),
            None => state.admission.clone().try_acquire_owned().ok(),
        };
        let service = AcpService::new(
            application.clone(),
            store.clone(),
            authenticated.clone(),
            v2_runtime_enabled,
        )
        .with_accepting(accepting.clone())
        .with_health(healthy.clone());
        match permit {
            Some(permit) => service.with_admission_permit(Arc::new(permit)),
            None => service.with_accepting(Arc::new(AtomicBool::new(false))),
        }
    })
    .with_options(ServerOptions {
        path: "/acp".to_owned(),
        cors: CorsOptions::disabled(),
        health_endpoint: false,
    })
    .into_router()
}

struct AdmissionCleanup {
    reservation: Arc<AdmissionReservation>,
    armed: bool,
}

impl AdmissionCleanup {
    fn new(
        permit: tokio::sync::OwnedSemaphorePermit,
        gate: tokio::sync::OwnedMutexGuard<()>,
    ) -> (Arc<AdmissionReservation>, Self) {
        let reservation = Arc::new(AdmissionReservation::new(permit, gate));
        (
            reservation.clone(),
            Self {
                reservation,
                armed: true,
            },
        )
    }

    fn reap_after(self, delay: std::time::Duration) {
        let _reaper = self.reap_when(async move {
            tokio::time::sleep(delay).await;
        });
    }

    fn reap_when(
        mut self,
        ready: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        self.armed = false;
        let reservation = self.reservation.clone();
        tokio::spawn(async move {
            ready.await;
            reservation.cancel();
        })
    }
}

impl Drop for AdmissionCleanup {
    fn drop(&mut self) {
        if self.armed {
            self.reservation.cancel();
        }
    }
}

async fn database_healthy(state: &RemoteState) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(state.store.pool()),
        )
        .await,
        Ok(Ok(1))
    )
}

fn retryable_response(status: StatusCode) -> Response {
    let mut response = status.into_response();
    response
        .headers_mut()
        .insert(http::header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

fn auth_response(error: AuthError) -> Response {
    match error {
        AuthError::MissingAuthorization | AuthError::InvalidAuthorization => {
            let mut response =
                (StatusCode::UNAUTHORIZED, "Bearer authorization is required").into_response();
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            response
        }
        AuthError::Unauthorized => StatusCode::FORBIDDEN.into_response(),
        AuthError::NoBindings | AuthError::DuplicateBinding => {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug)]
struct UpgradeIncompatible;

impl IntoResponse for UpgradeIncompatible {
    fn into_response(self) -> Response {
        let mut response = (
            StatusCode::UPGRADE_REQUIRED,
            "A valid WebSocket upgrade is required",
        )
            .into_response();
        response
            .headers_mut()
            .insert(UPGRADE, HeaderValue::from_static("websocket"));
        response
            .headers_mut()
            .insert(SEC_WEBSOCKET_VERSION, HeaderValue::from_static("13"));
        response
    }
}

fn classify_transport(
    method: &Method,
    headers: &HeaderMap,
) -> Result<TransportKind, UpgradeIncompatible> {
    if !websocket_intent(headers) {
        return Ok(TransportKind::StreamableHttp);
    }
    let valid = method == Method::GET
        && header_eq(headers, UPGRADE, "websocket")
        && header_contains_token(headers, CONNECTION, "upgrade")
        && header_eq(headers, SEC_WEBSOCKET_VERSION, "13")
        && headers
            .get(SEC_WEBSOCKET_KEY)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| STANDARD.decode(value).ok())
            .is_some_and(|nonce| nonce.len() == 16);
    if valid {
        Ok(TransportKind::WebSocket)
    } else {
        Err(UpgradeIncompatible)
    }
}

fn websocket_intent(headers: &HeaderMap) -> bool {
    headers.contains_key(UPGRADE)
        || header_contains_token(headers, CONNECTION, "upgrade")
        || headers.contains_key(SEC_WEBSOCKET_KEY)
        || headers.contains_key(SEC_WEBSOCKET_VERSION)
}

fn header_eq(headers: &HeaderMap, name: http::header::HeaderName, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn header_contains_token(
    headers: &HeaderMap,
    name: http::header::HeaderName,
    expected: &str,
) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case(expected))
        })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NdjsonRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Compatibility helper for existing in-process tests. The executable uses the
/// SDK `Stdio` component and all semantic dispatch still goes through
/// `dispatch_method` below.
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
        if let Some(payload) = handle_line(&line, application, &connection).await {
            writer
                .write_all(&serde_json::to_vec(&payload).expect("JSON value serializes"))
                .await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

async fn handle_line(
    line: &str,
    application: &crate::Application,
    connection: &ProtocolConnection,
) -> Option<Value> {
    let raw = match serde_json::from_str::<Value>(line) {
        Ok(raw) => raw,
        Err(_) => return Some(error_response(Value::Null, RpcError::parse_error())),
    };
    let Some(object) = raw.as_object() else {
        return Some(error_response(Value::Null, RpcError::invalid_request()));
    };
    let raw_id = object.get("id");
    let valid_id = raw_id.is_none_or(|id| id.is_null() || id.is_string() || id.is_number());
    let request_id = raw_id.filter(|_| valid_id).cloned().unwrap_or(Value::Null);
    let valid_envelope = object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && object.get("method").is_some_and(Value::is_string)
        && object
            .get("params")
            .is_none_or(|params| params.is_object() || params.is_array());
    let notification = raw_id.is_none();
    let request = serde_json::from_value::<NdjsonRequest>(raw);
    let Ok(request) = request else {
        return Some(error_response(request_id, RpcError::invalid_request()));
    };
    if !valid_envelope || !valid_id {
        return Some(error_response(request_id, RpcError::invalid_request()));
    }
    let response = dispatch_method(application, connection, &request.method, request.params).await;
    if notification {
        return None;
    }
    Some(match response {
        Ok(value) => serde_json::json!({"jsonrpc": "2.0", "id": request_id, "result": value}),
        Err(error) => error_response(request_id, RpcError::from(error)),
    })
}

#[derive(Debug)]
struct RpcError {
    code: i32,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "Parse error".to_owned(),
            data: None,
        }
    }

    fn invalid_request() -> Self {
        Self {
            code: -32600,
            message: "Invalid Request".to_owned(),
            data: None,
        }
    }
}

impl From<crate::ProtocolError> for RpcError {
    fn from(error: crate::ProtocolError) -> Self {
        match error {
            crate::ProtocolError::UnsupportedMethod(_) => Self {
                code: -32601,
                message: "Method not found".to_owned(),
                data: None,
            },
            crate::ProtocolError::InvalidPayload(_)
            | crate::ProtocolError::UnsupportedVersion(_)
            | crate::ProtocolError::V2Disabled
            | crate::ProtocolError::McpUnavailable => Self {
                code: -32602,
                message: "Invalid params".to_owned(),
                data: None,
            },
            crate::ProtocolError::UnexpectedResult(_) => Self {
                code: -32603,
                message: "Internal error".to_owned(),
                data: None,
            },
            crate::ProtocolError::StableApplication { code, .. } => Self {
                code: -32000,
                message: "application request failed".to_owned(),
                data: Some(serde_json::json!({"kiteframe_code": code})),
            },
            _ => Self {
                code: -32000,
                message: "application request failed".to_owned(),
                data: None,
            },
        }
    }
}

fn error_response(id: Value, error: RpcError) -> Value {
    let mut response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": error.code, "message": error.message}
    });
    if let Some(data) = error.data {
        response["error"]["data"] = data;
    }
    response
}

async fn dispatch_method(
    application: &crate::Application,
    connection: &ProtocolConnection,
    method: &str,
    params: Value,
) -> Result<Value, crate::ProtocolError> {
    dispatch_application(application, connection, method, params)
        .await
        .map(|outcome| outcome.value)
}

struct DispatchOutcome {
    value: Value,
}

async fn dispatch_application(
    application: &crate::Application,
    connection: &ProtocolConnection,
    method: &str,
    params: Value,
) -> Result<DispatchOutcome, crate::ProtocolError> {
    if params.as_object().is_some_and(|params| {
        params
            .keys()
            .any(|key| key == "principal" || key == "principalId")
    }) {
        return Err(crate::ProtocolError::InvalidPayload(
            "connection principal is supplied only by authenticated transport context".to_owned(),
        ));
    }
    if method == "initialize" {
        return connection
            .initialize(params)
            .map(|value| DispatchOutcome { value });
    }
    let is_session_method = method.starts_with("session/");
    let command = if is_session_method {
        connection.decode_session(method, params)
    } else {
        connection.decode_extension(method, params)
    }?;
    let closing_session = match &command {
        crate::ApplicationCommand::CloseSession(close) => Some(close.session_id.clone()),
        _ => None,
    };
    let result = application
        .dispatch(&connection.application_context(), command)
        .await?;
    if let crate::ApplicationResult::Session(session) = &result {
        connection.bind_session(session.view.id.clone());
    }
    if let Some(session_id) = closing_session {
        connection.clear_session(&session_id);
    }
    let value = if is_session_method {
        connection.encode_session_result(method, result)
    } else {
        connection.encode_extension_result(method, result)
    }?;
    Ok(DispatchOutcome { value })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        AdmissionCleanup, BearerAuthenticator, ConnectionAuthenticator, StaticBearerAuthenticator,
    };
    use crate::{PrincipalId, Secret};
    use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    async fn admission_reservation() -> (
        Arc<super::AdmissionReservation>,
        AdmissionCleanup,
        Arc<tokio::sync::Semaphore>,
        Arc<tokio::sync::Mutex<()>>,
    ) {
        let admission = Arc::new(tokio::sync::Semaphore::new(1));
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let permit = admission
            .clone()
            .try_acquire_owned()
            .expect("admission permit");
        let gate_guard = gate.clone().lock_owned().await;
        let (reservation, cleanup) = AdmissionCleanup::new(permit, gate_guard);
        (reservation, cleanup, admission, gate)
    }

    #[tokio::test]
    async fn cancellation_before_websocket_dispatch_releases_permit_and_gate() {
        let (reservation, cleanup, admission, gate) = admission_reservation().await;
        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
        let waiter_admission = admission.clone();
        let waiter = tokio::spawn(async move {
            waiting_tx.send(()).expect("signal waiter");
            let _gate = gate.lock_owned().await;
            assert!(reservation.claim().is_none());
            assert_eq!(waiter_admission.available_permits(), 1);
            waiter_admission
                .try_acquire_owned()
                .expect("permit must be restored before gate release")
        });

        waiting_rx.await.expect("waiter started");
        assert!(
            !waiter.is_finished(),
            "the reservation must still own the gate"
        );
        drop(cleanup);
        let permit = waiter.await.expect("admission waiter");
        assert_eq!(admission.available_permits(), 0);
        drop(permit);
        assert_eq!(admission.available_permits(), 1);
    }

    #[tokio::test]
    async fn factory_claim_before_cleanup_transfers_the_permit_exactly_once() {
        let (reservation, cleanup, admission, gate) = admission_reservation().await;

        let permit = reservation
            .claim_before_gate_release(|| {
                assert!(reservation.claim().is_none());
                assert_eq!(admission.available_permits(), 0);
                assert!(gate.clone().try_lock_owned().is_err());
                drop(cleanup);
            })
            .expect("factory claims reservation");

        assert!(reservation.claim().is_none());
        assert_eq!(admission.available_permits(), 0);
        assert!(gate.clone().try_lock_owned().is_ok());
        drop(permit);
        assert_eq!(admission.available_permits(), 1);
    }

    #[tokio::test]
    async fn cleanup_before_factory_makes_the_reservation_unclaimable() {
        let (reservation, cleanup, admission, gate) = admission_reservation().await;

        reservation.cancel_before_gate_release(|| {
            assert!(reservation.claim().is_none());
            assert_eq!(admission.available_permits(), 1);
            assert!(gate.clone().try_lock_owned().is_err());
        });
        drop(cleanup);

        assert!(reservation.claim().is_none());
        assert_eq!(admission.available_permits(), 1);
        assert!(gate.try_lock_owned().is_ok());
    }

    #[tokio::test]
    async fn late_factory_cannot_claim_the_next_websocket_reservation() {
        let (old_reservation, old_cleanup, admission, gate) = admission_reservation().await;
        drop(old_cleanup);

        let next_gate = gate.clone().lock_owned().await;
        let next_permit = admission
            .clone()
            .try_acquire_owned()
            .expect("next admission permit");
        let (next_reservation, next_cleanup) = AdmissionCleanup::new(next_permit, next_gate);

        assert!(old_reservation.claim().is_none());
        assert_eq!(admission.available_permits(), 0);
        let claimed = next_reservation
            .claim()
            .expect("next factory claims only its own reservation");
        drop(next_cleanup);
        assert_eq!(admission.available_permits(), 0);
        drop(claimed);
        assert_eq!(admission.available_permits(), 1);
    }

    #[tokio::test]
    async fn post_upgrade_reaper_wins_before_a_delayed_factory() {
        let (old_reservation, old_cleanup, admission, gate) = admission_reservation().await;
        let (reap_tx, reap_rx) = tokio::sync::oneshot::channel::<()>();
        let reaper = old_cleanup.reap_when(async move {
            reap_rx.await.expect("release orphan reaper");
        });
        let (next_waiting_tx, next_waiting_rx) = tokio::sync::oneshot::channel();
        let next_admission = admission.clone();
        let next_gate = gate.clone();
        let next = tokio::spawn(async move {
            next_waiting_tx.send(()).expect("signal next reservation");
            let gate_guard = next_gate.lock_owned().await;
            let permit = next_admission
                .clone()
                .try_acquire_owned()
                .expect("reaper restores capacity before releasing the gate");
            AdmissionCleanup::new(permit, gate_guard)
        });

        next_waiting_rx.await.expect("next reservation waiting");
        assert!(
            !next.is_finished(),
            "orphan reservation still owns the gate"
        );
        reap_tx.send(()).expect("run orphan reaper");
        reaper.await.expect("orphan reaper");
        let (next_reservation, next_cleanup) = next.await.expect("next reservation");

        assert!(old_reservation.claim().is_none());
        assert_eq!(admission.available_permits(), 0);
        let permit = next_reservation
            .claim()
            .expect("next factory claims its exact reservation");
        drop(next_cleanup);
        assert_eq!(admission.available_permits(), 0);
        drop(permit);
        assert_eq!(admission.available_permits(), 1);
    }

    #[test]
    fn static_bearer_authenticator_accepts_only_the_configured_token() {
        let auth =
            StaticBearerAuthenticator::new("token".to_owned(), PrincipalId::new("alice").unwrap());
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer token"));
        assert_eq!(auth.authenticate(&headers).unwrap().as_str(), "alice");
        headers.insert(AUTHORIZATION, HeaderValue::from_static("bearer token"));
        assert_eq!(auth.authenticate(&headers).unwrap().as_str(), "alice");
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert!(auth.authenticate(&headers).is_err());
    }

    #[test]
    fn bearer_binding_set_selects_the_authenticated_principal_without_debug_leakage() {
        let auth = BearerAuthenticator::new([
            (
                Secret::new("first-secret"),
                PrincipalId::new("alice").unwrap(),
            ),
            (
                Secret::new("second-secret"),
                PrincipalId::new("bob").unwrap(),
            ),
        ])
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer second-secret"),
        );
        assert_eq!(auth.authenticate(&headers).unwrap().as_str(), "bob");
        let debug = format!("{auth:?}");
        assert!(!debug.contains("first-secret"));
        assert!(!debug.contains("second-secret"));
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

    #[test]
    fn runtime_disabled_v2_is_reported_as_a_negotiation_error() {
        let sdk_error =
            serde_json::to_value(super::protocol_sdk_error(crate::ProtocolError::V2Disabled))
                .expect("serialize SDK error");
        assert_eq!(sdk_error["code"], -32602);

        let ndjson_error = super::RpcError::from(crate::ProtocolError::V2Disabled);
        assert_eq!(ndjson_error.code, -32602);
    }
}
