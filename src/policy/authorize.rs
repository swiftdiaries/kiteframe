use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;

use super::compile::{
    INPUT_CONTEXT_KEY, RULE_ID_ANNOTATION, UNCOMPARABLE_CONTEXT_KEY, capability_name, cedar_string,
    compile,
};
use super::input::{Argument, argument_at, input_matches};
use super::{PolicyEffect, PolicyError, PolicyRule, PolicyTarget, SessionPolicy};
use crate::{Capability, PolicyRevision, SessionId, ToolCatalogSnapshot};

#[derive(Clone, Debug)]
pub struct PolicyRequest {
    pub session: SessionId,
    pub qualified_name: String,
    pub descriptor_digest: String,
    pub capabilities: Vec<Capability>,
    pub arguments: serde_json::Value,
}
/// A decision and the policy revision it was evaluated against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PolicyEvaluation {
    pub decision: PolicyDecision,
    /// `None` when the authorizer holds an in-memory policy with no persisted revision.
    pub policy_revision: Option<PolicyRevision>,
}
impl From<PolicyDecision> for PolicyEvaluation {
    fn from(decision: PolicyDecision) -> Self {
        Self {
            decision,
            policy_revision: None,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
    Allow,
    Forbid { rule_ids: Vec<String> },
    NeedsPermission,
}
#[async_trait]
pub trait SessionAuthorizer: Send + Sync {
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyEvaluation, PolicyError>;
}
/// The reference model of a session policy, evaluated directly over the typed rules.
pub struct TypedAuthorizer {
    policy: SessionPolicy,
    tool_digests: BTreeMap<String, String>,
}
impl TypedAuthorizer {
    pub fn new(policy: SessionPolicy, catalog: &ToolCatalogSnapshot) -> Result<Self, PolicyError> {
        policy.validate(catalog)?;
        let tool_digests = catalog
            .tools()
            .iter()
            .map(|(name, tool)| (name.clone(), tool.descriptor_digest().to_owned()))
            .collect();
        Ok(Self {
            policy,
            tool_digests,
        })
    }

    fn applies(&self, rule: &PolicyRule, request: &PolicyRequest) -> bool {
        self.target_applies(rule, request)
            && input_matches(rule.effect, &rule.input, &request.arguments)
    }

    fn target_applies(&self, rule: &PolicyRule, request: &PolicyRequest) -> bool {
        match &rule.target {
            PolicyTarget::Tool { names } => {
                names.contains(&request.qualified_name)
                    && (rule.effect == PolicyEffect::Forbid
                        || self.tool_digests.get(&request.qualified_name)
                            == Some(&request.descriptor_digest))
            }
            PolicyTarget::Capability { names } => match rule.effect {
                PolicyEffect::Forbid => request
                    .capabilities
                    .iter()
                    .any(|capability| names.contains(capability)),
                PolicyEffect::Permit => {
                    !request.capabilities.is_empty()
                        && request
                            .capabilities
                            .iter()
                            .all(|capability| names.contains(capability))
                }
            },
        }
    }
}
#[async_trait]
impl SessionAuthorizer for TypedAuthorizer {
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyEvaluation, PolicyError> {
        let mut permits = false;
        let mut forbids = BTreeSet::new();
        for rule in &self.policy.rules {
            if self.applies(rule, request) {
                match rule.effect {
                    PolicyEffect::Permit => permits = true,
                    PolicyEffect::Forbid => {
                        forbids.insert(rule.rule_id.clone());
                    }
                }
            }
        }
        Ok(if !forbids.is_empty() {
            PolicyDecision::Forbid {
                rule_ids: forbids.into_iter().collect(),
            }
        } else if permits {
            PolicyDecision::Allow
        } else {
            PolicyDecision::NeedsPermission
        }
        .into())
    }
}

pub struct CedarAuthorizer {
    policy: cedar_policy::PolicySet,
    input_pointers: BTreeSet<String>,
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
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyEvaluation, PolicyError> {
        let (policy, revision, catalog) = self
            .store
            .current_policy_and_catalog(request.session.clone())
            .await
            .map_err(|error| PolicyError::Cedar(error.to_string()))?;
        let evaluation = CedarAuthorizer::new(&policy, &catalog)?
            .authorize(request)
            .await?;
        Ok(PolicyEvaluation {
            decision: evaluation.decision,
            policy_revision: Some(revision),
        })
    }
}
impl CedarAuthorizer {
    pub fn new(policy: &SessionPolicy, catalog: &ToolCatalogSnapshot) -> Result<Self, PolicyError> {
        let compiled = compile(policy, catalog)?;
        Ok(Self {
            policy: cedar_policy::PolicySet::from_str(&compiled.cedar)
                .map_err(|error| PolicyError::Cedar(error.to_string()))?,
            input_pointers: policy
                .rules
                .iter()
                .flat_map(|rule| rule.input.keys().cloned())
                .collect(),
        })
    }

    fn forbidding_rule_ids(&self, response: &cedar_policy::Response) -> Vec<String> {
        response
            .diagnostics()
            .reason()
            .filter_map(|id| self.policy.policy(id))
            .filter(|policy| policy.effect() == cedar_policy::Effect::Forbid)
            .filter_map(|policy| policy.annotation(RULE_ID_ANNOTATION).map(str::to_owned))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
#[async_trait]
impl SessionAuthorizer for CedarAuthorizer {
    async fn authorize(&self, request: &PolicyRequest) -> Result<PolicyEvaluation, PolicyError> {
        let cedar_error = |error: &dyn std::fmt::Display| PolicyError::Cedar(error.to_string());
        let identity = cedar_string(&format!(
            "{}#{}",
            request.qualified_name, request.descriptor_digest
        ));
        let principal = format!("Kiteframe::Session::\"{}\"", request.session.as_uuid())
            .parse()
            .map_err(|error: cedar_policy::ParseErrors| cedar_error(&error))?;
        let action = format!("Action::{identity}")
            .parse()
            .map_err(|error: cedar_policy::ParseErrors| cedar_error(&error))?;
        let resource = format!("Kiteframe::Tool::{identity}")
            .parse()
            .map_err(|error: cedar_policy::ParseErrors| cedar_error(&error))?;
        let capabilities = request
            .capabilities
            .iter()
            .map(capability_name)
            .collect::<Vec<_>>();
        let mut input = serde_json::Map::new();
        let mut uncomparable = Vec::new();
        for pointer in &self.input_pointers {
            match argument_at(&request.arguments, pointer) {
                Argument::Missing => {}
                Argument::Comparable(value) => {
                    input.insert(pointer.clone(), value.clone());
                }
                Argument::Uncomparable => uncomparable.push(pointer.as_str()),
            }
        }
        let context = serde_json::json!({
            "capabilities": capabilities,
            "tool": request.qualified_name,
            INPUT_CONTEXT_KEY: input,
            UNCOMPARABLE_CONTEXT_KEY: uncomparable,
        });
        let context = cedar_policy::Context::from_json_value(context, None)
            .map_err(|error| cedar_error(&error))?;
        let cedar_request = cedar_policy::Request::new(principal, action, resource, context, None)
            .map_err(|error| cedar_error(&error))?;
        let response = cedar_policy::Authorizer::new().is_authorized(
            &cedar_request,
            &self.policy,
            &cedar_policy::Entities::empty(),
        );
        if let Some(error) = response.diagnostics().errors().next() {
            return Err(cedar_error(error));
        }
        Ok(match response.decision() {
            cedar_policy::Decision::Allow => PolicyDecision::Allow,
            cedar_policy::Decision::Deny => match self.forbidding_rule_ids(&response) {
                rule_ids if rule_ids.is_empty() => PolicyDecision::NeedsPermission,
                rule_ids => PolicyDecision::Forbid { rule_ids },
            },
        }
        .into())
    }
}
