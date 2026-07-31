use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, Request, State,
        rejection::{BytesRejection, JsonRejection},
    },
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kiteframe_contract::{
    AdmissionRequest, CapabilityCatalog, CapabilityGrantSet, InvocationId, InvocationOutcome,
    InvocationRequest, StatusRequest, TraceContext,
};
use kiteframe_provider::{
    AdmissionService, AuthorizationBackend, InvocationStatusContext, InvocationStore,
};
use tower_http::limit::RequestBodyLimitLayer;
use url::Url;

use crate::{
    HttpErrorKind, ProviderHttpError, ProviderPrincipalVerifier, ProviderRequestContext,
    auth::{ProviderAuthState, authenticate_provider_request},
    trace::{ProviderTraceState, extract_trace_context},
};

#[async_trait]
pub trait ProviderHttpServices: Send + Sync {
    fn observe_catalog_response(&self, _not_modified: bool) {}

    async fn catalog(
        &self,
        context: &ProviderRequestContext,
    ) -> Result<CapabilityCatalog, ProviderHttpError>;

    async fn observe_admission(
        &self,
        _context: &ProviderRequestContext,
        _request: &AdmissionRequest,
    ) -> Result<(), ProviderHttpError> {
        Ok(())
    }

    async fn invoke(
        &self,
        context: &ProviderRequestContext,
        request: InvocationRequest,
    ) -> Result<InvocationOutcome, ProviderHttpError>;

    async fn observe_status(
        &self,
        _request: &AuthenticatedStatusRequest,
    ) -> Result<(), ProviderHttpError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct ProviderHttpState {
    services: Arc<dyn ProviderHttpServices>,
    status_store: Arc<dyn InvocationStore>,
    admission_plane: Option<Arc<EnforcedAdmissionPlane>>,
    origin: Url,
}

pub struct EnforcedAdmissionPlane {
    service: Arc<AdmissionService>,
    authorization: Arc<dyn AuthorizationBackend>,
}

impl EnforcedAdmissionPlane {
    pub fn new(
        service: Arc<AdmissionService>,
        authorization: Arc<dyn AuthorizationBackend>,
    ) -> Self {
        Self {
            service,
            authorization,
        }
    }

    async fn admit(
        &self,
        context: &ProviderRequestContext,
        request: AdmissionRequest,
    ) -> Result<CapabilityGrantSet, ProviderHttpError> {
        let principals = context
            .authenticated_admission_context(&request, self.service.issued_at())
            .map_err(|diagnostic| {
                ProviderHttpError::new(HttpErrorKind::IdentityMismatch, diagnostic)
            })?;
        self.service
            .admit(request, principals, self.authorization.as_ref())
            .await
            .map_err(|diagnostic| ProviderHttpError::new(HttpErrorKind::Conflict, diagnostic))
    }
}

impl ProviderHttpState {
    pub fn new(
        services: Arc<dyn ProviderHttpServices>,
        status_store: Arc<dyn InvocationStore>,
    ) -> Self {
        Self {
            services,
            status_store,
            admission_plane: None,
            origin: Url::parse("https://provider.invalid")
                .expect("static default provider origin is valid"),
        }
    }

    pub fn with_admission_plane(mut self, plane: Arc<EnforcedAdmissionPlane>) -> Self {
        self.admission_plane = Some(plane);
        self
    }

    pub fn with_origin(mut self, value: &str) -> Result<Self, String> {
        self.origin = crate::ServerBindConfig::origin(value)?;
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct AuthenticatedStatusRequest {
    request: StatusRequest,
    context: ProviderRequestContext,
    status_context: InvocationStatusContext,
}

impl AuthenticatedStatusRequest {
    fn try_new(
        request: StatusRequest,
        context: ProviderRequestContext,
    ) -> Result<Self, ProviderHttpError> {
        let principals = context.principals();
        let human = principals.human();
        let workload = principals.workload();
        let status_context = InvocationStatusContext::try_new(
            human.tenant_ref().as_str(),
            human.human_ref().as_str(),
            workload.workload_ref().as_str(),
            workload.run_ref().as_str(),
            human.mapped_actor().as_str(),
            workload.mapped_agent().as_str(),
            workload.task_ref().as_str(),
            workload.session_ref().as_str(),
            workload.admission_ref().as_str(),
        )
        .map_err(|_| ProviderHttpError::identity_mismatch())?;
        Ok(Self {
            request,
            context,
            status_context,
        })
    }

    pub fn request(&self) -> &StatusRequest {
        &self.request
    }

    pub fn context(&self) -> &ProviderRequestContext {
        &self.context
    }

    pub fn status_context(&self) -> &InvocationStatusContext {
        &self.status_context
    }
}

pub fn provider_router(
    state: ProviderHttpState,
    principal_verifier: Arc<dyn ProviderPrincipalVerifier>,
) -> Router {
    let origin = state.origin.clone();
    Router::new()
        .route("/v1/capability-catalog", get(catalog))
        .route("/v1/capability-admissions", post(admit))
        .route(
            "/v1/capability-invocations/{identifier}",
            post(invoke).get(status),
        )
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(RequestBodyLimitLayer::new(1_048_576))
        .layer(from_fn(normalize_body_limit_response))
        .layer(from_fn(enforce_exact_contract))
        .layer(from_fn_with_state(
            ProviderAuthState::new(principal_verifier.clone()),
            authenticate_provider_request,
        ))
        .layer(from_fn_with_state(origin, enforce_runtime_origin))
        .layer(from_fn_with_state(
            ProviderTraceState::new(principal_verifier),
            extract_trace_context,
        ))
        .with_state(state)
}

async fn enforce_runtime_origin(
    State(expected): State<Url>,
    request: Request,
    next: Next,
) -> Result<Response, ProviderHttpError> {
    if ["forwarded", "x-forwarded-host", "x-forwarded-proto"]
        .iter()
        .any(|name| request.headers().contains_key(*name))
    {
        return Err(ProviderHttpError::identity_mismatch());
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let origin = origin
            .to_str()
            .ok()
            .and_then(|value| crate::ServerBindConfig::origin(value).ok());
        if origin.as_ref() != Some(&expected) {
            return Err(ProviderHttpError::identity_mismatch());
        }
    }
    if let Some(host) = request.headers().get(header::HOST) {
        let host = host
            .to_str()
            .ok()
            .and_then(|value| value.parse::<axum::http::uri::Authority>().ok());
        if !host.is_some_and(|host| authority_matches_origin(&host, &expected)) {
            return Err(ProviderHttpError::identity_mismatch());
        }
    }
    if request.uri().scheme().is_some() || request.uri().authority().is_some() {
        let absolute = Url::parse(&request.uri().to_string()).ok().and_then(|url| {
            crate::ServerBindConfig::origin(&url.origin().ascii_serialization()).ok()
        });
        if absolute.as_ref() != Some(&expected) {
            return Err(ProviderHttpError::identity_mismatch());
        }
    }
    let response = next.run(request).await;
    if response.status().is_redirection() && response.status() != StatusCode::NOT_MODIFIED {
        return Err(ProviderHttpError::identity_mismatch());
    }
    Ok(response)
}

fn authority_matches_origin(authority: &axum::http::uri::Authority, origin: &Url) -> bool {
    let expected_port = origin.port_or_known_default();
    let scheme_default_port = match origin.scheme() {
        "https" => Some(443),
        "http" => Some(80),
        _ => None,
    };
    let actual_port = authority.port_u16().or(scheme_default_port);
    origin
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case(authority.host()))
        && actual_port == expected_port
}

async fn catalog(
    State(state): State<ProviderHttpState>,
    Extension(context): Extension<ProviderRequestContext>,
    headers: HeaderMap,
) -> Result<Response, ProviderHttpError> {
    let catalog = state.services.catalog(&context).await?;
    let etag = format!("\"{}\"", catalog.catalog_digest());
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').map(str::trim).any(|value| value == etag))
    {
        state.services.observe_catalog_response(true);
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    state.services.observe_catalog_response(false);
    Ok(([(header::ETAG, etag)], Json(catalog)).into_response())
}

async fn admit(
    State(state): State<ProviderHttpState>,
    Extension(context): Extension<ProviderRequestContext>,
    payload: Result<Json<AdmissionRequest>, JsonRejection>,
) -> Result<Response, ProviderHttpError> {
    let request = native_json(payload)?;
    validate_admission_principals(&context, &request)?;
    validate_trace(&context, request.trace_context())?;
    state.services.observe_admission(&context, &request).await?;
    let plane = state.admission_plane.as_ref().ok_or_else(|| {
        ProviderHttpError::new(
            HttpErrorKind::ServiceFailure,
            kiteframe_contract::Diagnostic::error(
                kiteframe_contract::DiagnosticCode::RuntimeConstruction,
                kiteframe_contract::DiagnosticCategory::Runtime,
                kiteframe_contract::DiagnosticStage::Runtime,
                "provider admission enforcement plane is not configured",
            ),
        )
    })?;
    plane
        .admit(&context, request)
        .await
        .map(|value| Json(value).into_response())
}

async fn invoke(
    State(state): State<ProviderHttpState>,
    Extension(context): Extension<ProviderRequestContext>,
    Path(name): Path<String>,
    payload: Result<Json<InvocationRequest>, JsonRejection>,
) -> Result<Response, ProviderHttpError> {
    let request = native_json(payload)?;
    if request.capability().name().as_str() != name
        || request.admission_id() != context.principals().workload().admission_ref()
    {
        return Err(ProviderHttpError::identity_mismatch());
    }
    validate_trace(&context, request.trace_context())?;
    state
        .services
        .invoke(&context, request)
        .await
        .map(|value| Json(value).into_response())
}

async fn status(
    State(state): State<ProviderHttpState>,
    Extension(context): Extension<ProviderRequestContext>,
    Path(invocation_id): Path<String>,
) -> Result<Response, ProviderHttpError> {
    let invocation_id =
        InvocationId::new(invocation_id).map_err(|_| ProviderHttpError::malformed())?;
    let request = StatusRequest::new(invocation_id, context.trace_context().clone());
    let expected_invocation_id = request.invocation_id().clone();
    let request = AuthenticatedStatusRequest::try_new(request, context)?;
    let response = state
        .status_store
        .status(request.request(), request.status_context())
        .await
        .map_err(|diagnostic| {
            ProviderHttpError::new(HttpErrorKind::IdentityMismatch, diagnostic)
        })?
        .portable()
        .map_err(|diagnostic| ProviderHttpError::new(HttpErrorKind::ServiceFailure, diagnostic))?;
    state.services.observe_status(&request).await?;
    response
        .validate_invocation_id(&expected_invocation_id)
        .map_err(|diagnostic| ProviderHttpError::new(HttpErrorKind::ServiceFailure, diagnostic))?;
    Ok(Json(response).into_response())
}

fn native_json<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ProviderHttpError> {
    payload.map(|Json(value)| value).map_err(|rejection| {
        if rejection.body_text().contains("length limit exceeded")
            || matches!(
                rejection,
                JsonRejection::BytesRejection(BytesRejection::FailedToBufferBody(_))
            )
        {
            ProviderHttpError::payload_too_large()
        } else {
            ProviderHttpError::malformed()
        }
    })
}

fn validate_admission_principals(
    context: &ProviderRequestContext,
    request: &AdmissionRequest,
) -> Result<(), ProviderHttpError> {
    let principals = context.principals();
    if principals.human().mapped_actor() != request.actor()
        || principals.workload().mapped_agent() != request.agent()
        || principals.workload().task_ref() != request.task()
        || principals.workload().session_ref() != request.session()
        || request.delegation_ancestry().edges().iter().any(|edge| {
            !context.verifies_delegation_agent(edge.parent_agent())
                || !context.verifies_delegation_agent(edge.child_agent())
        })
        || request
            .delegation_ancestry()
            .edges()
            .last()
            .is_some_and(|edge| edge.child_agent() != request.agent())
    {
        return Err(ProviderHttpError::identity_mismatch());
    }
    Ok(())
}

async fn enforce_exact_contract(
    request: Request,
    next: Next,
) -> Result<Response, ProviderHttpError> {
    let method = request.method();
    let path = request.uri().path();
    let known_path = path == "/v1/capability-catalog"
        || path == "/v1/capability-admissions"
        || invocation_identifier(path).is_some();
    let allowed = matches!(
        (method, path),
        (&Method::GET, "/v1/capability-catalog") | (&Method::POST, "/v1/capability-admissions")
    ) || (invocation_identifier(path).is_some()
        && matches!(method, &Method::GET | &Method::POST));
    if known_path && !allowed {
        return Err(ProviderHttpError::method_not_allowed());
    }
    if method == Method::GET && request_has_body(&request) {
        return Err(ProviderHttpError::payload_too_large());
    }
    Ok(next.run(request).await)
}

fn request_has_body(request: &Request) -> bool {
    use http_body::Body as _;

    if request.headers().contains_key(header::TRANSFER_ENCODING) {
        return true;
    }
    let content_lengths = request.headers().get_all(header::CONTENT_LENGTH);
    if content_lengths
        .iter()
        .any(|value| value.to_str().ok() != Some("0"))
    {
        return true;
    }
    let hint = request.body().size_hint();
    hint.lower() != 0 || hint.upper().is_none_or(|upper| upper != 0)
}

async fn normalize_body_limit_response(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ProviderHttpError::payload_too_large().into_response();
    }
    response
}

fn invocation_identifier(path: &str) -> Option<&str> {
    path.strip_prefix("/v1/capability-invocations/")
        .filter(|identifier| !identifier.is_empty() && !identifier.contains('/'))
}

fn validate_trace(
    context: &ProviderRequestContext,
    request_trace: &TraceContext,
) -> Result<(), ProviderHttpError> {
    if context.trace_context() != request_trace {
        return Err(ProviderHttpError::identity_mismatch());
    }
    Ok(())
}

async fn not_found() -> ProviderHttpError {
    ProviderHttpError::not_found()
}

async fn method_not_allowed() -> ProviderHttpError {
    ProviderHttpError::method_not_allowed()
}
