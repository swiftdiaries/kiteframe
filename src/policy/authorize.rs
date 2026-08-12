use std::str::FromStr;

use super::{CompiledPolicy, PolicyEffect, PolicyError, PolicyTarget, SessionPolicy};
use crate::{Capability, SessionId};
use std::sync::Arc;
use async_trait::async_trait;
#[derive(Clone, Debug)]
pub struct PolicyRequest {
    pub session: SessionId,
    pub qualified_name: String,
    pub descriptor_digest: String,
    pub capabilities: Vec<Capability>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
    Allow,
    Forbid { rule_ids: Vec<String> },
    NeedsPermission,
}
#[async_trait]
pub trait SessionAuthorizer: Send + Sync {
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyDecision, PolicyError>;
}
pub struct TypedAuthorizer {
    policy: SessionPolicy,
}
impl TypedAuthorizer {
    #[must_use]
    pub const fn new(policy: SessionPolicy) -> Self {
        Self { policy }
    }
}
#[async_trait]
impl SessionAuthorizer for TypedAuthorizer {
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyDecision, PolicyError> {
        let mut permits = false;
        let mut forbids = Vec::new();
        for rule in &self.policy.rules {
            let applies = match &rule.target {
                PolicyTarget::Tool { names } => names.contains(&request.qualified_name),
                PolicyTarget::Capability { names } => request
                    .capabilities
                    .iter()
                    .any(|capability| names.contains(capability)),
            };
            if applies {
                match rule.effect {
                    PolicyEffect::Permit => permits = true,
                    PolicyEffect::Forbid => forbids.push(rule.rule_id.clone()),
                }
            }
        }
        Ok(if !forbids.is_empty() {
            PolicyDecision::Forbid { rule_ids: forbids }
        } else if permits {
            PolicyDecision::Allow
        } else {
            PolicyDecision::NeedsPermission
        })
    }
}

pub struct CedarAuthorizer {
    policy: cedar_policy::PolicySet,
    typed: TypedAuthorizer,
}

pub struct PersistedAuthorizer {
    store: Arc<crate::PostgresStore>,
}

impl PersistedAuthorizer {
    #[must_use]
    pub const fn new(store: Arc<crate::PostgresStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl SessionAuthorizer for PersistedAuthorizer {
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyDecision, PolicyError> {
        let policy = self.store.current_session_policy(request.session.clone()).await
            .map_err(|error| PolicyError::Cedar(error.to_string()))?;
        let catalog = self.store.current_catalog(request.session.clone()).await
            .map_err(|error| PolicyError::Cedar(error.to_string()))?;
        let compiled = crate::policy::compile(&policy, &catalog)?;
        CedarAuthorizer::new(&compiled, policy)?.authorize(request).await
    }
}
impl CedarAuthorizer {
    pub fn new(
        compiled: &CompiledPolicy,
        typed_policy: SessionPolicy,
    ) -> Result<Self, PolicyError> {
        Ok(Self {
            policy: cedar_policy::PolicySet::from_str(&compiled.cedar)
                .map_err(|error| PolicyError::Cedar(error.to_string()))?,
            typed: TypedAuthorizer::new(typed_policy),
        })
    }
}
#[async_trait]
impl SessionAuthorizer for CedarAuthorizer {
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyDecision, PolicyError> {
        if let PolicyDecision::Forbid { rule_ids } = self.typed.authorize(request).await? {
            return Ok(PolicyDecision::Forbid { rule_ids });
        }
        let identity = format!("{}#{}", request.qualified_name, request.descriptor_digest);
        let principal = format!("Kiteframe::Session::\"{}\"", request.session.as_uuid())
            .parse()
            .map_err(|error: cedar_policy::ParseErrors| PolicyError::Cedar(error.to_string()))?;
        let action = format!("Action::\"{identity}\"")
            .parse()
            .map_err(|error: cedar_policy::ParseErrors| PolicyError::Cedar(error.to_string()))?;
        let resource = format!("Kiteframe::Tool::\"{identity}\"")
            .parse()
            .map_err(|error: cedar_policy::ParseErrors| PolicyError::Cedar(error.to_string()))?;
        let cedar_request = cedar_policy::Request::new(
            principal,
            action,
            resource,
            cedar_policy::Context::empty(),
            None,
        )
        .map_err(|error| PolicyError::Cedar(error.to_string()))?;
        let answer = cedar_policy::Authorizer::new().is_authorized(
            &cedar_request,
            &self.policy,
            &cedar_policy::Entities::empty(),
        );
        Ok(match answer.decision() {
            cedar_policy::Decision::Allow => PolicyDecision::Allow,
            cedar_policy::Decision::Deny => PolicyDecision::NeedsPermission,
        })
    }
}
