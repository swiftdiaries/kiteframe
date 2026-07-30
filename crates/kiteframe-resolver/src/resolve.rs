use std::collections::{BTreeMap, BTreeSet};

use kiteframe_contract::{
    CapabilityLock, CatalogIdentity, CompilationDecision, CompilationReport, CompilationWarning,
    ComponentKind, ComponentMetadataCatalog, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, FeatureId, FeatureSet, IrSchemaVersion, PackageIdentity, ResolvedAgent,
    ResolvedAgentParts, ResolvedCapabilityRequirement, ResolvedContentCaptureRequirement,
    ResolvedSubagent, RuntimeBinding, RuntimeTargetDescriptor, Sha256Digest,
};
use kiteframe_core::{AgentPackage, canonical_json, hash_domain};
use semver::{Version, VersionReq};

use crate::{
    feature::negotiate_features,
    model::{component_unresolved, require_component_kind, resolve_models},
    verify_lock,
};

const BINDING_DIGEST_DOMAIN: &[u8] = b"runtime-binding";

#[derive(Clone, Debug)]
pub struct ResolutionInput {
    pub package: AgentPackage,
    pub lock: CapabilityLock,
    pub child_locks: BTreeMap<PackageIdentity, CapabilityLock>,
    pub binding: RuntimeBinding,
    pub target: RuntimeTargetDescriptor,
    pub components: ComponentMetadataCatalog,
}

pub fn resolve_agent(input: ResolutionInput) -> Result<ResolvedAgent, Vec<Diagnostic>> {
    validate_runtime_inputs(&input.binding, &input.target, &input.components)?;

    let declared_children = collect_child_identities(&input.package);
    if input
        .child_locks
        .keys()
        .any(|identity| !declared_children.contains(identity))
    {
        return Err(vec![lock_stale(
            "child lock map contains an undeclared package identity",
        )]);
    }

    let binding_bytes = canonical_json(&input.binding).map_err(|error| vec![error])?;
    let binding_digest = hash_domain(BINDING_DIGEST_DOMAIN, [binding_bytes.as_slice()]);
    let mut seen = BTreeSet::new();
    resolve_package(
        &input.package,
        &input.lock,
        &input.child_locks,
        &input.binding,
        &input.target,
        &input.components,
        binding_digest,
        &mut seen,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_package(
    package: &AgentPackage,
    lock: &CapabilityLock,
    child_locks: &BTreeMap<PackageIdentity, CapabilityLock>,
    binding: &RuntimeBinding,
    target: &RuntimeTargetDescriptor,
    components: &ComponentMetadataCatalog,
    binding_digest: Sha256Digest,
    seen: &mut BTreeSet<PackageIdentity>,
) -> Result<ResolvedAgent, Vec<Diagnostic>> {
    let identity = package.manifest().metadata.clone();
    if !seen.insert(identity.clone()) {
        return Err(vec![package_invalid(
            "resolved package identities must be unique",
        )]);
    }

    verify_lock(package, lock, None)?;

    let required_features = convert_features(
        package
            .manifest()
            .spec
            .features
            .required
            .iter()
            .map(|feature| feature.as_str()),
    )?;
    let optional_features = convert_features(
        package
            .manifest()
            .spec
            .features
            .optional
            .iter()
            .map(|feature| feature.as_str()),
    )?;
    let negotiation = negotiate_features(
        &required_features,
        &optional_features,
        &target.supported_features,
    )?;

    let model_resolution = resolve_models(&package.manifest().spec.models, binding, components)?;
    let models = model_resolution.models;
    let mut warnings = model_resolution.warnings;
    for omitted in &negotiation.omitted_optional {
        warnings.push(CompilationWarning {
            code: "KF-FEAT-OPTIONAL-OMITTED".to_owned(),
            message: format!("optional feature {omitted} is unsupported"),
        });
    }

    let (capability_requirements, capability_warnings) = resolve_capabilities(package, lock)?;
    warnings.extend(capability_warnings);
    let content_capture = resolve_content_capture(package, binding, components)?;

    let mut subagents = Vec::new();
    for delegation in &package.manifest().spec.delegation {
        let Some(child) = package.subagents().get(&delegation.agent) else {
            return Err(vec![package_invalid(
                "declared child package is absent after package validation",
            )]);
        };
        let child_identity = child.manifest().metadata.clone();
        let Some(child_lock) = child_locks.get(&child_identity) else {
            return Err(vec![lock_stale(format!(
                "exact child lock is missing for {} {}",
                child_identity.name, child_identity.version
            ))]);
        };
        if child_lock.package_portable_digest != *child.portable_digest() {
            return Err(vec![lock_stale(
                "child lock identity does not correspond to the declared child package",
            )]);
        }
        let child_resolved = resolve_package(
            child,
            child_lock,
            child_locks,
            binding,
            target,
            components,
            binding_digest,
            seen,
        )?;
        subagents.push(ResolvedSubagent {
            package_identity: child_identity,
            delegation: delegation.clone(),
            resolved_digest: *child_resolved.resolved_digest(),
        });
    }

    let compilation_report = CompilationReport {
        warnings,
        decisions: vec![
            CompilationDecision {
                subject: "features".to_owned(),
                outcome: format!(
                    "{} required and {} optional enabled",
                    required_features.len(),
                    negotiation.enabled_optional.len()
                ),
            },
            CompilationDecision {
                subject: "models".to_owned(),
                outcome: format!("{} roles resolved", models.len()),
            },
        ],
    };

    ResolvedAgent::try_new(ResolvedAgentParts {
        schema_version: IrSchemaVersion::V1Alpha1,
        package_identity: identity,
        portable_digest: *package.portable_digest(),
        lock_digest: lock.lock_digest,
        catalog_identity: CatalogIdentity {
            name: lock.catalog_identity.clone(),
            revision: lock.catalog_revision.clone(),
        },
        catalog_digest: lock.catalog_digest,
        binding_digest,
        prompts: package.prompt_assets().clone(),
        skills: package.skill_assets().clone(),
        models,
        capability_requirements,
        subagents,
        required_features,
        optional_features: negotiation.enabled_optional,
        content_capture,
        compilation_report,
    })
    .map_err(|message| vec![package_invalid(message)])
}

fn validate_runtime_inputs(
    binding: &RuntimeBinding,
    target: &RuntimeTargetDescriptor,
    components: &ComponentMetadataCatalog,
) -> Result<(), Vec<Diagnostic>> {
    if binding.metadata.runtime != target.target || components.target != target.target {
        return Err(vec![component_unresolved(
            "binding, runtime target, and component catalog target must match exactly",
        )]);
    }

    let mut diagnostics = Vec::new();
    for symbol in &binding.spec.components.middleware {
        if let Err(error) = require_component_kind(components, symbol, ComponentKind::Middleware) {
            diagnostics.push(error);
        }
    }
    for (symbol, expected) in [
        (
            binding.spec.components.backend.as_ref(),
            ComponentKind::Backend,
        ),
        (
            binding.spec.components.checkpointer.as_ref(),
            ComponentKind::Checkpointer,
        ),
        (
            binding.spec.components.authority_provider.as_ref(),
            ComponentKind::AuthorityProvider,
        ),
        (
            binding.spec.components.admitted_tool_registry.as_ref(),
            ComponentKind::AdmittedToolRegistry,
        ),
        (
            binding.spec.components.harness_profile.as_ref(),
            ComponentKind::HarnessProfile,
        ),
    ] {
        if let Some(symbol) = symbol
            && let Err(error) = require_component_kind(components, symbol, expected)
        {
            diagnostics.push(error);
        }
    }
    for (symbol, expected) in [
        (
            &binding.spec.capability_provider,
            ComponentKind::CapabilityProvider,
        ),
        (&binding.spec.audit_sink, ComponentKind::AuditSink),
    ] {
        if let Err(error) = require_component_kind(components, symbol, expected) {
            diagnostics.push(error);
        }
    }
    diagnostics.sort();
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

fn resolve_content_capture(
    package: &AgentPackage,
    binding: &RuntimeBinding,
    components: &ComponentMetadataCatalog,
) -> Result<ResolvedContentCaptureRequirement, Vec<Diagnostic>> {
    let portable = &package.manifest().spec.observability.content_capture;
    let Some(deployment) = binding
        .spec
        .content_capture
        .as_ref()
        .filter(|policy| policy.enabled)
    else {
        return Ok(ResolvedContentCaptureRequirement::default());
    };

    if !portable.allowed {
        return Err(vec![package_invalid(
            "runtime binding cannot enable content capture disallowed by the package",
        )]);
    }
    if !deployment
        .classifications
        .is_subset(&portable.classifications)
    {
        return Err(vec![package_invalid(
            "runtime binding cannot broaden package content-capture classifications",
        )]);
    }

    let mut diagnostics = Vec::new();
    for (symbol, expected) in [
        (&deployment.redaction_policy, ComponentKind::RedactionPolicy),
        (&deployment.retention_policy, ComponentKind::RetentionPolicy),
        (&deployment.access_policy, ComponentKind::AccessPolicy),
        (
            &deployment.encrypted_content_store,
            ComponentKind::EncryptedContentStore,
        ),
    ] {
        if let Err(error) = require_component_kind(components, symbol, expected) {
            diagnostics.push(error);
        }
    }
    if !diagnostics.is_empty() {
        diagnostics.sort();
        return Err(diagnostics);
    }

    Ok(ResolvedContentCaptureRequirement {
        allowed: true,
        classifications: deployment.classifications.iter().copied().collect(),
    })
}

fn resolve_capabilities(
    package: &AgentPackage,
    lock: &CapabilityLock,
) -> Result<(Vec<ResolvedCapabilityRequirement>, Vec<CompilationWarning>), Vec<Diagnostic>> {
    let mut resolved = Vec::new();
    let mut warnings = Vec::new();
    let mut matched = BTreeSet::new();

    for requirement in &package.manifest().spec.capabilities {
        let version_requirement = VersionReq::parse(requirement.version.as_str())
            .map_err(|_| vec![package_invalid("capability requirement version is invalid")])?;
        let selected = lock.capabilities.iter().find(|capability| {
            capability.identity().name() == &requirement.name
                && Version::parse(capability.identity().version().as_str())
                    .is_ok_and(|version| version_requirement.matches(&version))
        });
        if let Some(capability) = selected {
            matched.insert(capability.identity().clone());
            resolved.push(
                ResolvedCapabilityRequirement::try_new(
                    capability.clone(),
                    requirement.required,
                    requirement
                        .resources
                        .iter()
                        .map(|resource| resource.as_str().to_owned())
                        .collect(),
                )
                .map_err(|message| vec![package_invalid(message)])?,
            );
        } else if requirement.required {
            return Err(vec![lock_stale(format!(
                "required capability {} is absent from exact lock",
                requirement.name
            ))]);
        } else {
            warnings.push(CompilationWarning {
                code: DiagnosticCode::CatalogIncompatible.as_str().to_owned(),
                message: format!(
                    "optional capability {} {} is unavailable",
                    requirement.name, requirement.version
                ),
            });
        }
    }

    if lock
        .capabilities
        .iter()
        .any(|capability| !matched.contains(capability.identity()))
    {
        return Err(vec![lock_stale(
            "capability lock contains a selection not declared by the package",
        )]);
    }

    Ok((resolved, warnings))
}

fn convert_features<'a>(
    features: impl IntoIterator<Item = &'a str>,
) -> Result<FeatureSet, Vec<Diagnostic>> {
    features
        .into_iter()
        .map(|feature| {
            FeatureId::new(feature).map_err(|_| {
                vec![package_invalid(
                    "validated package contains an invalid feature identifier",
                )]
            })
        })
        .collect()
}

fn collect_child_identities(package: &AgentPackage) -> BTreeSet<PackageIdentity> {
    let mut identities = BTreeSet::new();
    for child in package.subagents().values() {
        identities.insert(child.manifest().metadata.clone());
        identities.extend(collect_child_identities(child));
    }
    identities
}

fn package_invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Resolve,
        message.into(),
    )
}

fn lock_stale(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::LockStale,
        DiagnosticCategory::Lock,
        DiagnosticStage::Resolve,
        message.into(),
    )
}
