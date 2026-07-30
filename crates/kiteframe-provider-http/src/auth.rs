use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderName},
    middleware::Next,
    response::Response,
};
use kiteframe_contract::{AgentRef, Diagnostic, TraceContext};
use kiteframe_provider::{VerifiedHumanPrincipal, VerifiedWorkloadPrincipal};

use crate::{VerifiedProviderPrincipals, response::ProviderHttpError};

#[derive(Clone, Debug)]
pub struct VerifiedHumanAuthentication {
    principal: VerifiedHumanPrincipal,
    consumed_headers: Vec<HeaderName>,
}

impl VerifiedHumanAuthentication {
    pub fn new(
        principal: VerifiedHumanPrincipal,
        consumed_headers: impl IntoIterator<Item = HeaderName>,
    ) -> Self {
        Self {
            principal,
            consumed_headers: consumed_headers.into_iter().collect(),
        }
    }

    fn into_parts(self) -> (VerifiedHumanPrincipal, Vec<HeaderName>) {
        (self.principal, self.consumed_headers)
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedWorkloadAuthentication {
    principal: VerifiedWorkloadPrincipal,
    consumed_headers: Vec<HeaderName>,
    delegation_agents: BTreeSet<AgentRef>,
}

impl VerifiedWorkloadAuthentication {
    pub fn new(
        principal: VerifiedWorkloadPrincipal,
        consumed_headers: impl IntoIterator<Item = HeaderName>,
    ) -> Self {
        let delegation_agents = BTreeSet::from([principal.mapped_agent().clone()]);
        Self {
            principal,
            consumed_headers: consumed_headers.into_iter().collect(),
            delegation_agents,
        }
    }

    pub fn with_delegation_agents(mut self, agents: impl IntoIterator<Item = AgentRef>) -> Self {
        self.delegation_agents.extend(agents);
        self
    }

    fn into_parts(
        self,
    ) -> (
        VerifiedWorkloadPrincipal,
        Vec<HeaderName>,
        BTreeSet<AgentRef>,
    ) {
        (
            self.principal,
            self.consumed_headers,
            self.delegation_agents,
        )
    }
}

#[async_trait]
pub trait ProviderPrincipalVerifier: Send + Sync {
    fn observe_trace(&self, _trace_context: &TraceContext) {}

    fn allow_baggage_key(&self, key: &str) -> bool {
        TraceContext::ALLOWED_BAGGAGE_KEYS.contains(&key)
    }

    async fn verify_human(
        &self,
        headers: &HeaderMap,
    ) -> Result<VerifiedHumanAuthentication, Diagnostic>;

    async fn verify_workload(
        &self,
        headers: &HeaderMap,
    ) -> Result<VerifiedWorkloadAuthentication, Diagnostic>;
}

#[derive(Clone, Debug)]
pub struct ProviderRequestContext {
    principals: VerifiedProviderPrincipals,
    trace_context: TraceContext,
    verified_delegation_agents: BTreeSet<AgentRef>,
}

impl ProviderRequestContext {
    pub fn principals(&self) -> &VerifiedProviderPrincipals {
        &self.principals
    }

    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }

    pub(crate) fn verifies_delegation_agent(&self, agent: &AgentRef) -> bool {
        self.verified_delegation_agents.contains(agent)
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
    let human_authentication = state
        .verifier
        .verify_human(request.headers())
        .await
        .map_err(|_| ProviderHttpError::authentication_failed())?;
    let workload_authentication = state
        .verifier
        .verify_workload(request.headers())
        .await
        .map_err(|_| ProviderHttpError::authentication_failed())?;
    let (human, human_headers) = human_authentication.into_parts();
    let (workload, workload_headers, verified_delegation_agents) =
        workload_authentication.into_parts();
    let principals = VerifiedProviderPrincipals::new(human, workload);
    if principals.human().tenant_ref() != principals.workload().tenant_ref() {
        return Err(ProviderHttpError::identity_mismatch());
    }

    strip_untrusted_headers(
        request.headers_mut(),
        human_headers.into_iter().chain(workload_headers),
    );
    request.extensions_mut().insert(ProviderRequestContext {
        principals,
        trace_context,
        verified_delegation_agents,
    });
    Ok(next.run(request).await)
}

fn strip_untrusted_headers(
    headers: &mut HeaderMap,
    consumed_headers: impl IntoIterator<Item = HeaderName>,
) {
    let mut remove = headers
        .keys()
        .filter(|name| !handler_header_allowlisted(name))
        .cloned()
        .collect::<Vec<_>>();
    remove.extend(consumed_headers);
    for name in remove {
        headers.remove(name);
    }
}

fn handler_header_allowlisted(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "accept"
            | "content-length"
            | "content-type"
            | "host"
            | "if-none-match"
            | "origin"
            | "user-agent"
    )
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderName, HeaderValue, header};

    use super::strip_untrusted_headers;

    #[test]
    fn strips_declared_credentials_and_every_non_transport_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"digest\""),
        );
        for name in ["x-signature", "x-jwt", "client-assertion", "x-opaque"] {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static("opaque"),
            );
        }

        strip_untrusted_headers(
            &mut headers,
            [
                HeaderName::from_static("x-signature"),
                HeaderName::from_static("x-jwt"),
                HeaderName::from_static("client-assertion"),
            ],
        );

        assert!(headers.contains_key(header::CONTENT_TYPE));
        assert!(headers.contains_key(header::IF_NONE_MATCH));
        assert!(!headers.contains_key("x-signature"));
        assert!(!headers.contains_key("x-jwt"));
        assert!(!headers.contains_key("client-assertion"));
        assert!(!headers.contains_key("x-opaque"));
    }
}
