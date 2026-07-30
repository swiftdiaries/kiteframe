use std::{collections::BTreeMap, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue},
    middleware::Next,
    response::Response,
};
use kiteframe_contract::TraceContext;

use crate::{ProviderHttpError, ProviderPrincipalVerifier};

#[derive(Clone)]
pub(crate) struct ProviderTraceState {
    verifier: Arc<dyn ProviderPrincipalVerifier>,
}

impl ProviderTraceState {
    pub(crate) fn new(verifier: Arc<dyn ProviderPrincipalVerifier>) -> Self {
        Self { verifier }
    }
}

pub(crate) async fn extract_trace_context(
    State(state): State<ProviderTraceState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ProviderHttpError> {
    let trace_context = parse_trace_context(request.headers(), state.verifier.as_ref())?;
    state.verifier.observe_trace(&trace_context);
    request.extensions_mut().insert(trace_context.clone());
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "traceparent",
        HeaderValue::from_str(trace_context.traceparent())
            .map_err(|_| ProviderHttpError::trace_invalid())?,
    );
    if let Some(tracestate) = trace_context.tracestate() {
        response.headers_mut().insert(
            "tracestate",
            HeaderValue::from_str(tracestate).map_err(|_| ProviderHttpError::trace_invalid())?,
        );
    }
    Ok(response)
}

fn parse_trace_context(
    headers: &HeaderMap,
    verifier: &dyn ProviderPrincipalVerifier,
) -> Result<TraceContext, ProviderHttpError> {
    let traceparent = header_value(headers, "traceparent")?;
    let tracestate = headers
        .get("tracestate")
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| ProviderHttpError::trace_invalid())
        })
        .transpose()?;
    let baggage = parse_baggage(headers, verifier)?;
    TraceContext::try_new(traceparent, tracestate, baggage)
        .map_err(|_| ProviderHttpError::trace_invalid())
}

fn header_value(headers: &HeaderMap, name: &'static str) -> Result<String, ProviderHttpError> {
    headers
        .get(name)
        .ok_or_else(ProviderHttpError::missing_trace_context)?
        .to_str()
        .map(str::to_owned)
        .map_err(|_| ProviderHttpError::trace_invalid())
}

fn parse_baggage(
    headers: &HeaderMap,
    verifier: &dyn ProviderPrincipalVerifier,
) -> Result<BTreeMap<String, String>, ProviderHttpError> {
    let Some(raw) = headers.get("baggage") else {
        return Ok(BTreeMap::new());
    };
    let raw = raw
        .to_str()
        .map_err(|_| ProviderHttpError::trace_invalid())?;
    let mut baggage = BTreeMap::new();
    for member in raw.split(',') {
        let Some((key, value)) = member.trim().split_once('=') else {
            return Err(ProviderHttpError::trace_invalid());
        };
        let key = key.trim();
        if sensitive_name(key) {
            return Err(ProviderHttpError::trace_invalid());
        }
        if TraceContext::ALLOWED_BAGGAGE_KEYS.contains(&key) && verifier.allow_baggage_key(key) {
            baggage.insert(key.to_owned(), value.trim().to_owned());
        }
    }
    Ok(baggage)
}

fn sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "token",
        "secret",
        "credential",
        "api-key",
        "api_key",
        "claim",
        "prompt",
        "argument",
        "result",
        "evidence",
    ]
    .iter()
    .any(|sensitive| name.contains(sensitive))
}
