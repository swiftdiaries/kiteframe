#![forbid(unsafe_code)]

mod binding;
mod capability;
mod catalog;
mod component;
mod diagnostic;
mod digest;
mod feature;
mod ir;
mod lock;
mod manifest;
mod package;
mod schema;
mod service;

pub use binding::{
    BindingContentCapturePolicy, RegistrySymbol, RuntimeBinding, RuntimeBindingMetadata,
    RuntimeBindingSpec, RuntimeTarget, TypedComponentSymbols,
};
pub use capability::{
    ApprovalRequirement, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityErrorDescriptor, CapabilityIdentity, CapabilityReleaseVersion,
    ConfirmationRequirement, ConsentRequirement, EffectClassification, EvidenceRequirement,
    ExecutionMode, FreshnessRequirement, IdempotencyRequirement, IdempotencyScope,
    JsonSchema2020_12, NonEmptySet, PreconditionDescriptor, PreconditionKind,
    ResourceSelectorSchema,
};
pub use catalog::{CapabilityCatalog, CatalogIdentity};
pub use component::{
    ComponentKind, ComponentMetadata, ComponentMetadataCatalog, ModelLatencyClass, ModelMetadata,
    ModelModality, ResidencyClass, RuntimeTargetDescriptor,
};
pub use diagnostic::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticSeverity, DiagnosticStage,
    RetryClass, SafeMessage, SourceRange,
};
pub use digest::Sha256Digest;
pub use feature::{
    CompilationDecision, CompilationReport, CompilationWarning, FeatureId, FeatureNegotiation,
    FeatureSet,
};
pub use ir::{
    IrSchemaVersion, ResolvedAgent, ResolvedAgentParts, ResolvedCapabilityRequirement,
    ResolvedContentCaptureRequirement, ResolvedModelRequirement, ResolvedSubagent,
};
pub use lock::{CapabilityLock, LockSchemaVersion, LockedCapability};
pub use manifest::{
    AgentManifest, AgentName, AgentSpec, CapabilityName, CapabilityRequirement, CapabilityVersion,
    ContentCaptureRequirement, DataClassification, DelegationRequirement, Feature,
    FeatureRequirements, LatencyClass, ModelCapability, ModelRequirement, ModelRole,
    ObservabilityRequirements, PackageIdentity, PackageVersion, PromptRequirement,
    ResourceSelector,
};
pub use package::{InvalidPackagePath, PackagePath, ValidatedTextAsset};
pub use schema::{
    AGENT_API_VERSION, AgentKind, AgentSchemaVersion, BINDING_API_VERSION, BindingSchemaVersion,
    RuntimeBindingKind,
};
pub use service::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, AuthorityRevision,
    AuthorityRevisionSet, BaggageCorrelationId, CapabilityDenial, CapabilityGrantSet,
    CapabilityGrantSetParts, CatalogRequest, DelegationAncestry, EffectiveCapabilityGrant,
    EffectiveCapabilityGrantParts, EvidenceReferences, IdempotencyKey, InvocationId,
    InvocationOutcome, InvocationRequest, InvocationStatus, NormalizedResourceSelector,
    PolicyRevision, RequestedCapability, RequiredEvidence, SessionRef, StableCapabilityError,
    StatusFirstDiagnostic, Suspension, TaskRef, Timestamp, TraceContext,
};
