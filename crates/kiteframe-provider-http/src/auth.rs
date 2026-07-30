use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use kiteframe_contract::{Diagnostic, TraceContext};
use kiteframe_provider::{VerifiedHumanPrincipal, VerifiedWorkloadPrincipal};

use crate::{VerifiedProviderPrincipals, response::ProviderHttpError};

#[async_trait]
pub trait ProviderPrincipalVerifier: Send + Sync {
    fn observe_trace(&self, _trace_context: &TraceContext) {}

    fn allow_baggage_key(&self, key: &str) -> bool {
        TraceContext::ALLOWED_BAGGAGE_KEYS.contains(&key)
    }

    async fn verify_human(&self, headers: &HeaderMap)
    -> Result<VerifiedHumanPrincipal, Diagnostic>;

    async fn verify_workload(
        &self,
        headers: &HeaderMap,
    ) -> Result<VerifiedWorkloadPrincipal, Diagnostic>;
}

#[derive(Clone, Debug)]
pub struct ProviderRequestContext {
    principals: VerifiedProviderPrincipals,
    trace_context: TraceContext,
}

impl ProviderRequestContext {
    pub fn principals(&self) -> &VerifiedProviderPrincipals {
        &self.principals
    }

    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }
}

#[derive(Clone)]
pub(crate) struct ProviderAuthState {
    verifier: Arc<dyn ProviderPrincipalVerifier>,
}

impl ProviderAuthState {
    pub(crate) fn new(verifier: Arc<dyn ProviderPrincipalVerifier>) -> Self {
        Self { verifier }
    }
}

pub(crate) async fn authenticate_provider_request(
    State(state): State<ProviderAuthState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ProviderHttpError> {
    let trace_context = request
        .extensions()
        .get::<TraceContext>()
        .cloned()
        .ok_or_else(ProviderHttpError::missing_trace_context)?;
    let human = state
        .verifier
        .verify_human(request.headers())
        .await
        .map_err(|_| ProviderHttpError::authentication_failed())?;
    let workload = state
        .verifier
        .verify_workload(request.headers())
        .await
        .map_err(|_| ProviderHttpError::authentication_failed())?;
    let principals = VerifiedProviderPrincipals::new(human, workload);
    if principals.human().tenant_ref() != principals.workload().tenant_ref() {
        return Err(ProviderHttpError::identity_mismatch());
    }

    strip_credentials(request.headers_mut());
    request.extensions_mut().insert(ProviderRequestContext {
        principals,
        trace_context,
    });
    Ok(next.run(request).await)
}

fn strip_credentials(headers: &mut HeaderMap) {
    let credential_headers = headers
        .keys()
        .filter(|name| credential_bearing_header(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for name in credential_headers {
        headers.remove(name);
    }
}

fn credential_bearing_header(name: &str) -> bool {
    name.contains("authorization")
        || name.contains("cookie")
        || name.contains("token")
        || name.contains("secret")
        || name.contains("credential")
        || name.contains("api-key")
        || name.contains("api_key")
        || name.contains("claims")
}

#[cfg(test)]
mod tests {
    use super::credential_bearing_header;

    #[test]
    fn custom_human_and_workload_authorization_headers_are_credential_bearing() {
        assert!(credential_bearing_header("x-human-authorization"));
        assert!(credential_bearing_header("x-workload-authorization"));
        assert!(!credential_bearing_header("if-none-match"));
        assert!(!credential_bearing_header("traceparent"));
    }
}
