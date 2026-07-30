#![forbid(unsafe_code)]

mod admission;
mod authority;
mod authorization;
mod invocation;
mod operation;
mod principal;

pub use admission::{
    AdmissionService, AdmissionServiceConfig, AuthorityDomain, AuthorityPlane, AuthoritySource,
    PersistedAdmission,
};
pub use authority::{AuthorityTerm, EffectiveGrantSubset, intersect_authority};
pub use authorization::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AuthorizationBackend,
    AuthorizationDecision, DecisionRef, InvocationAuthorizationRequest,
    NarrowedAuthorizationConditions, SafeDenialReason, require_current_authorization,
};
pub use invocation::{
    InMemoryInvocationAdmissionStore, InvocationAdmission, InvocationAdmissionStore,
    InvocationCheckpointIssuer, InvocationClock, InvocationEventSink, InvocationEvidenceProvider,
    InvocationService, ResumeRequest, VerifiedEvidence,
};
pub use operation::{
    CapabilityOperation, InvocationContext, OperationFailure, OperationRegistry, Precondition,
};
pub use principal::{
    AuthenticatedInvocationContext, HumanPrincipalRef, PortableInvocationRefs,
    ProviderPrincipalVerifier, RunRef, TenantRef, VerifiedHumanPrincipal,
    VerifiedProviderPrincipals, VerifiedWorkloadPrincipal, WorkloadPrincipalRef,
    correlate_principals,
};
