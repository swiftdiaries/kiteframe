#![forbid(unsafe_code)]

mod binding;
mod diagnostic;
mod manifest;
mod package;
mod schema;

pub use binding::{
    BindingContentCapturePolicy, RegistrySymbol, RuntimeBinding, RuntimeBindingMetadata,
    RuntimeBindingSpec, RuntimeTarget, TypedComponentSymbols,
};
pub use diagnostic::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticSeverity, DiagnosticStage,
    RetryClass, SafeMessage, SourceRange,
};
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
