#![forbid(unsafe_code)]

mod admission;
mod audit;
mod authority;
mod authorization;
mod invocation;
mod operation;
mod principal;
mod resource;
mod status;

pub use admission::{
    AdmissionService, AdmissionServiceConfig, AuthorityDomain, AuthorityPlane, AuthoritySource,
    PersistedAdmission,
};
pub use audit::{
    AuditRecord, AuditSink, AuthorizationAuditRecord, DurableAuditReceipt, OutcomeAuditKind,
    OutcomeAuditRecord, PreconditionRef, SpanId, TraceId,
};
pub use authority::{AuthorityTerm, EffectiveGrantSubset, intersect_authority};
pub use authorization::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AuthorizationBackend,
    AuthorizationDecision, DecisionRef, InvocationAuthorizationRequest,
    NarrowedAuthorizationConditions, SafeDenialReason, require_current_authorization,
};
pub use invocation::{
    EffectAuditDigests, EffectEnforcementPlane, InMemoryInvocationAdmissionStore,
    InvocationAdmission, InvocationAdmissionStore, InvocationCheckpointIssuer, InvocationClock,
    InvocationEventSink, InvocationEvidenceProvider, InvocationService, ResumeRequest,
    VerifiedEvidence,
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
pub use resource::{
    intersect_resource_selectors, resource_selector_is_subset,
    validate_concrete_resource_selector,
};
pub use status::{
    AbandonmentAuthorization, IdempotencyScopeValue, InMemoryInvocationStore, InvocationAuditLink,
    InvocationAuditLinkKind, InvocationReservation, InvocationReservationInput, InvocationState,
    InvocationStatus, InvocationStatusContext, InvocationStore, InvocationStoreClock,
    InvocationTransition, ReservationKind, StatusSafeError, StatusSafeResult, StatusState,
    StoredInvocation, SystemInvocationStoreClock, TransitionAuditRecord,
};
