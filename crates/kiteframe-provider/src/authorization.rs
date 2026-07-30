use async_trait::async_trait;
use kiteframe_contract::{
    AuthorityRevisionSet, CapabilityIdentity, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, NormalizedResourceSelector, PreconditionDescriptor, Sha256Digest, Timestamp,
};

use crate::AuthenticatedInvocationContext;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DecisionRef(String);

impl DecisionRef {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("authorization decision reference is required".to_owned());
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeDenialReason {
    CapabilityDenied,
    PrincipalDenied,
    ResourceDenied,
    StaleAuthority,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NarrowedAuthorizationConditions {
    resources: Vec<NormalizedResourceSelector>,
    expires_at: Timestamp,
    required_preconditions: Vec<PreconditionDescriptor>,
}

impl NarrowedAuthorizationConditions {
    pub fn new(
        mut resources: Vec<NormalizedResourceSelector>,
        expires_at: Timestamp,
        mut required_preconditions: Vec<PreconditionDescriptor>,
    ) -> Result<Self, String> {
        resources.sort();
        resources.dedup();
        if resources.is_empty() {
            return Err("authorized resources must not be empty".to_owned());
        }
        required_preconditions.sort();
        required_preconditions.dedup();
        Ok(Self {
            resources,
            expires_at,
            required_preconditions,
        })
    }

    pub fn resources(&self) -> &[NormalizedResourceSelector] {
        &self.resources
    }

    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    pub fn required_preconditions(&self) -> &[PreconditionDescriptor] {
        &self.required_preconditions
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationDecision {
    Allow {
        decision_ref: DecisionRef,
        authority_revisions: AuthorityRevisionSet,
        decided_at: Timestamp,
        narrowed_conditions: NarrowedAuthorizationConditions,
    },
    Deny {
        reason: SafeDenialReason,
        decision_ref: DecisionRef,
    },
}

impl AuthorizationDecision {
    pub fn allow(
        decision_ref: impl Into<String>,
        authority_revisions: AuthorityRevisionSet,
        decided_at: Timestamp,
        narrowed_conditions: NarrowedAuthorizationConditions,
    ) -> Result<Self, String> {
        Ok(Self::Allow {
            decision_ref: DecisionRef::new(decision_ref)?,
            authority_revisions,
            decided_at,
            narrowed_conditions,
        })
    }

    pub fn deny(decision_ref: impl Into<String>, reason: SafeDenialReason) -> Result<Self, String> {
        Ok(Self::Deny {
            reason,
            decision_ref: DecisionRef::new(decision_ref)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct AdmissionAuthorizationRequest {
    principals: AuthenticatedInvocationContext,
    capability: CapabilityIdentity,
    selected_resource: NormalizedResourceSelector,
    loaded_authority_revisions: AuthorityRevisionSet,
}

impl AdmissionAuthorizationRequest {
    pub fn new(
        principals: AuthenticatedInvocationContext,
        capability: CapabilityIdentity,
        selected_resource: NormalizedResourceSelector,
        loaded_authority_revisions: AuthorityRevisionSet,
    ) -> Self {
        Self {
            principals,
            capability,
            selected_resource,
            loaded_authority_revisions,
        }
    }

    pub fn principals(&self) -> &AuthenticatedInvocationContext {
        &self.principals
    }

    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }

    pub fn selected_resource(&self) -> &NormalizedResourceSelector {
        &self.selected_resource
    }

    pub fn loaded_authority_revisions(&self) -> &AuthorityRevisionSet {
        &self.loaded_authority_revisions
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionAuthorizationResult {
    admissible: Vec<CapabilityIdentity>,
}

impl AdmissionAuthorizationResult {
    pub fn new(mut admissible: Vec<CapabilityIdentity>) -> Self {
        admissible.sort();
        admissible.dedup();
        Self { admissible }
    }

    pub fn admissible(&self) -> &[CapabilityIdentity] {
        &self.admissible
    }
}

#[derive(Clone, Debug)]
pub struct InvocationAuthorizationRequest {
    principals: AuthenticatedInvocationContext,
    capability: CapabilityIdentity,
    selected_resource: NormalizedResourceSelector,
    grant_digest: Sha256Digest,
    loaded_authority_revisions: AuthorityRevisionSet,
}

impl InvocationAuthorizationRequest {
    pub fn new(
        principals: AuthenticatedInvocationContext,
        capability: CapabilityIdentity,
        selected_resource: NormalizedResourceSelector,
        grant_digest: Sha256Digest,
        loaded_authority_revisions: AuthorityRevisionSet,
    ) -> Self {
        Self {
            principals,
            capability,
            selected_resource,
            grant_digest,
            loaded_authority_revisions,
        }
    }

    pub fn principals(&self) -> &AuthenticatedInvocationContext {
        &self.principals
    }

    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }

    pub fn selected_resource(&self) -> &NormalizedResourceSelector {
        &self.selected_resource
    }

    pub fn grant_digest(&self) -> &Sha256Digest {
        &self.grant_digest
    }

    pub fn loaded_authority_revisions(&self) -> &AuthorityRevisionSet {
        &self.loaded_authority_revisions
    }
}

#[async_trait]
pub trait AuthorizationBackend: Send + Sync {
    async fn list_admissible(
        &self,
        request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, Diagnostic>;

    async fn check(
        &self,
        request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, Diagnostic>;

    async fn revisions(&self) -> Result<AuthorityRevisionSet, Diagnostic>;
}

/// Performs a fresh invocation-time check and rejects a deny or stale allow.
pub async fn require_current_authorization(
    backend: &dyn AuthorizationBackend,
    request: &InvocationAuthorizationRequest,
) -> Result<AuthorizationDecision, Diagnostic> {
    let current_revisions = backend.revisions().await?;
    match backend.check(request).await? {
        allow @ AuthorizationDecision::Allow { .. } => {
            let AuthorizationDecision::Allow {
                authority_revisions,
                ..
            } = &allow
            else {
                unreachable!("allow pattern was established")
            };
            if authority_revisions == &current_revisions {
                Ok(allow)
            } else {
                Err(Diagnostic::error(
                    DiagnosticCode::PolicyStale,
                    DiagnosticCategory::Authorization,
                    DiagnosticStage::Invoke,
                    "authorization decision used stale authority revisions",
                ))
            }
        }
        AuthorizationDecision::Deny { .. } => Err(Diagnostic::error(
            DiagnosticCode::InvocationDenied,
            DiagnosticCategory::Authorization,
            DiagnosticStage::Invoke,
            "current invocation authorization denied",
        )),
    }
}
