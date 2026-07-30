use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Extension, Json, Router,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{BytesRejection, JsonRejection},
    },
    http::{HeaderMap, StatusCode, header},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kiteframe_contract::{
    AdmissionRequest, CapabilityCatalog, CapabilityGrantSet, InvocationId, InvocationOutcome,
    InvocationRequest, InvocationStatus, StatusRequest, TraceContext,
};

use crate::{
    ProviderHttpError, ProviderPrincipalVerifier, ProviderRequestContext,
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

    async fn admit(
        &self,
        context: &ProviderRequestContext,
        request: AdmissionRequest,
    ) -> Result<CapabilityGrantSet, ProviderHttpError>;

    async fn invoke(
        &self,
        context: &ProviderRequestContext,
        request: InvocationRequest,
    ) -> Result<InvocationOutcome, ProviderHttpError>;

    async fn status(
        &self,
        context: &ProviderRequestContext,
        request: StatusRequest,
    ) -> Result<InvocationStatus, ProviderHttpError>;
}

#[derive(Clone)]
pub struct ProviderHttpState {
    services: Arc<dyn ProviderHttpServices>,
}

impl ProviderHttpState {
    pub fn new(services: Arc<dyn ProviderHttpServices>) -> Self {
        Self { services }
    }
}

pub fn provider_router(
    state: ProviderHttpState,
    principal_verifier: Arc<dyn ProviderPrincipalVerifier>,
) -> Router {
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
        .layer(from_fn_with_state(
            ProviderAuthState::new(principal_verifier.clone()),
            authenticate_provider_request,
        ))
        .layer(from_fn_with_state(
            ProviderTraceState::new(principal_verifier),
            extract_trace_context,
        ))
        .with_state(state)
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
    state
        .services
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
    state
        .services
        .status(&context, request)
        .await
        .map(|value| Json(value).into_response())
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
    {
        return Err(ProviderHttpError::identity_mismatch());
    }
    Ok(())
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
