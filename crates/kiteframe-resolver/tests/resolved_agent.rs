use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    BindingContentCapturePolicy, CapabilityCatalog, CatalogIdentity, ComponentKind,
    ComponentMetadataCatalog, DataClassification, FeatureId, FeatureSet, ModelLatencyClass,
    RegistrySymbol, ResidencyClass, RuntimeTargetDescriptor, Sha256Digest, Timestamp,
};
use kiteframe_core::{PackageLimits, load_package};
use kiteframe_resolver::{
    CandidatePolicy, ResolutionInput, lock_package, resolve_agent, validate_catalog,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn features(values: impl IntoIterator<Item = &'static str>) -> FeatureSet {
    values
        .into_iter()
        .map(|value| FeatureId::new(value).unwrap())
        .collect()
}

fn components() -> ComponentMetadataCatalog {
    serde_json::from_slice(
        &fs::read(fixture("components/deepagents-test.json")).expect("component fixture"),
    )
    .unwrap()
}

fn empty_catalog() -> kiteframe_resolver::ValidatedCatalog {
    let catalog = CapabilityCatalog::try_new(
        CatalogIdentity {
            name: "empty".to_owned(),
            revision: "v1".to_owned(),
        },
        Timestamp::new(100),
        Some(Timestamp::new(200)),
        Vec::new(),
    )
    .unwrap();
    validate_catalog(&serde_json::to_vec(&catalog).unwrap()).unwrap()
}

fn package(manifest: &str, child: Option<&str>) -> kiteframe_core::AgentPackage {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir_all(directory.path().join("prompts")).unwrap();
    fs::write(directory.path().join("agent.yaml"), manifest).unwrap();
    fs::write(directory.path().join("prompts/system.md"), "System prompt").unwrap();
    if let Some(child_manifest) = child {
        fs::create_dir_all(directory.path().join("agents/child/prompts")).unwrap();
        fs::write(
            directory.path().join("agents/child/agent.yaml"),
            child_manifest,
        )
        .unwrap();
        fs::write(
            directory.path().join("agents/child/prompts/system.md"),
            "Child prompt",
        )
        .unwrap();
    }
    load_package(directory.path(), PackageLimits::V1).unwrap()
}

fn root_manifest(required_feature: &str, content_capture: bool) -> String {
    format!(
        r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: {{ name: support, version: 0.1.0 }}
spec:
  prompt: {{ system: prompts/system.md }}
  models:
    primary:
      capabilities: [text, tool-calling, structured-output]
      minContextTokens: 64000
      maxLatencyClass: interactive
      residency: global
    fast:
      capabilities: [text, tool-calling, structured-output]
      minContextTokens: 32000
      maxLatencyClass: interactive
      residency: global
      required: false
  features:
    required: [{required_feature}]
    optional: [kiteframe.capability.deferred@1]
  observability:
    contentCapture:
      allowed: {content_capture}
      classifications: [confidential]
"#
    )
}

fn child_manifest() -> &'static str {
    r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: child, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text] }
"#
}

fn parent_with_child_manifest() -> &'static str {
    r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: parent, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text] }
  delegation:
    - agent: agents/child/agent.yaml
      capabilities: []
"#
}

fn binding() -> kiteframe_contract::RuntimeBinding {
    serde_json::from_value(json!({
        "apiVersion": "kiteframe.dev/binding/v1alpha1",
        "kind": "RuntimeBinding",
        "metadata": {"runtime": "deepagents"},
        "spec": {
            "models": {
                "primary": "models.anthropic.sonnet",
                "fast": "models.anthropic.haiku"
            },
            "components": {
                "middleware": ["middleware.tenant-context"],
                "backend": "backends.workspace",
                "checkpointer": "checkpointers.durable",
                "harnessProfile": "profiles.deepagents"
            },
            "capabilityProvider": "capability-providers.primary",
            "auditSink": "audit-sinks.ledger"
        }
    }))
    .unwrap()
}

fn resolution_fixture() -> ResolutionInput {
    let package = package(
        &root_manifest("kiteframe.capability.point-of-use-auth@1", true),
        None,
    );
    let lock = lock_package(&package, &empty_catalog(), CandidatePolicy::AllowAll).unwrap();
    ResolutionInput {
        package,
        lock,
        child_locks: BTreeMap::new(),
        binding: binding(),
        target: RuntimeTargetDescriptor {
            target: serde_json::from_value(json!("deepagents")).unwrap(),
            supported_features: features([
                "kiteframe.capability.point-of-use-auth@1",
                "kiteframe.capability.deferred@1",
            ]),
            target_digest: Sha256Digest::from_bytes([7; Sha256Digest::BYTE_LENGTH]),
        },
        components: components(),
    }
}

fn locked_resolution_fixture() -> ResolutionInput {
    let manifest = format!(
        "{}  capabilities:\n    - {{ name: cases.read, version: ^1.0, required: true, resources: [tenant:support] }}\n",
        root_manifest("kiteframe.capability.point-of-use-auth@1", true)
    );
    let package = package(&manifest, None);
    let catalog =
        validate_catalog(&fs::read(fixture("catalogs/support-v1.json")).unwrap()).unwrap();
    let lock = lock_package(&package, &catalog, CandidatePolicy::AllowAll).unwrap();
    let mut input = resolution_fixture();
    input.package = package;
    input.lock = lock;
    input
}

#[test]
fn resolved_requirement_retains_the_exact_verified_lock_entry() {
    let input = locked_resolution_fixture();
    let expected = input.lock.capabilities[0].clone();
    let expected_catalog_digest = input.lock.catalog_digest;
    let resolved = resolve_agent(input).unwrap();
    let requirement = &resolved.capability_requirements()[0];

    assert_eq!(resolved.catalog_identity().name, "support");
    assert_eq!(resolved.catalog_identity().revision, "v1");
    assert_eq!(resolved.catalog_digest(), &expected_catalog_digest);
    assert_eq!(requirement.locked_capability(), &expected);
    assert_eq!(requirement.descriptor(), expected.descriptor());
    assert_eq!(
        requirement.descriptor_digest(),
        expected.descriptor_digest()
    );
    assert_eq!(
        requirement.input_schema_digest(),
        expected.input_schema_digest()
    );
    assert_eq!(
        requirement.output_schema_digest(),
        expected.output_schema_digest()
    );
    assert_eq!(
        requirement.stable_error_set_digest(),
        expected.stable_error_set_digest()
    );
    assert_eq!(
        requirement.safety_metadata_digest(),
        expected.safety_metadata_digest()
    );
}

fn refresh_lock_digest(lock: &mut kiteframe_contract::CapabilityLock) {
    let mut material = serde_json::to_value(&*lock).unwrap();
    material.as_object_mut().unwrap().remove("lockDigest");
    lock.lock_digest = Sha256Digest::from_bytes(
        Sha256::digest(serde_json_canonicalizer::to_vec(&material).unwrap()).into(),
    );
}

fn binding_capture() -> BindingContentCapturePolicy {
    BindingContentCapturePolicy {
        enabled: true,
        classifications: BTreeSet::from([DataClassification::Confidential]),
        redaction_policy: RegistrySymbol::new("redaction-policies.default").unwrap(),
        retention_policy: RegistrySymbol::new("retention-policies.default").unwrap(),
        access_policy: RegistrySymbol::new("access-policies.default").unwrap(),
        encrypted_content_store: RegistrySymbol::new("content-stores.encrypted").unwrap(),
    }
}

#[test]
fn unsupported_required_feature_stops_resolution() {
    let package = package(
        &root_manifest("kiteframe.capability.point-of-use-auth@2", true),
        None,
    );
    let lock = lock_package(&package, &empty_catalog(), CandidatePolicy::AllowAll).unwrap();
    let mut input = resolution_fixture();
    input.package = package;
    input.lock = lock;

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-FEAT-001");
}

#[test]
fn resolution_rejects_lock_feature_drift_before_target_negotiation() {
    let mut input = resolution_fixture();
    input
        .lock
        .resolved_features
        .remove(&FeatureId::new("kiteframe.capability.deferred@1").unwrap());
    refresh_lock_digest(&mut input.lock);

    let errors = resolve_agent(input).unwrap_err();

    assert!(errors.iter().any(|error| {
        error.code.as_str() == "KF-LOCK-001"
            && error.message.as_str() == "package requested features do not match capability lock"
    }));
    assert!(
        errors
            .iter()
            .all(|error| error.code.as_str() != "KF-FEAT-001")
    );
}

#[test]
fn optional_model_falls_back_only_when_primary_satisfies_every_constraint() {
    let mut input = resolution_fixture();
    input
        .components
        .components
        .remove(&RegistrySymbol::new("models.anthropic.haiku").unwrap());

    let resolved = resolve_agent(input).unwrap();

    assert_eq!(
        resolved.models()[&serde_json::from_value(json!("fast")).unwrap()]
            .symbol()
            .as_str(),
        "models.anthropic.sonnet"
    );
    assert!(
        resolved
            .compilation_report()
            .warnings
            .iter()
            .any(|entry| { entry.code == "KF-MODEL-OPTIONAL-FALLBACK" })
    );
}

#[test]
fn optional_model_is_omitted_when_primary_does_not_satisfy_its_constraints() {
    let manifest = root_manifest("kiteframe.capability.point-of-use-auth@1", true)
        .replace("minContextTokens: 32000", "minContextTokens: 250000");
    let package = package(&manifest, None);
    let lock = lock_package(&package, &empty_catalog(), CandidatePolicy::AllowAll).unwrap();
    let mut input = resolution_fixture();
    input.package = package;
    input.lock = lock;
    input
        .components
        .components
        .remove(&RegistrySymbol::new("models.anthropic.haiku").unwrap());

    let resolved = resolve_agent(input).unwrap();
    let fast = serde_json::from_value(json!("fast")).unwrap();

    assert!(!resolved.models().contains_key(&fast));
    assert!(
        resolved
            .compilation_report()
            .warnings
            .iter()
            .any(|entry| { entry.code == "KF-MODEL-OPTIONAL-OMITTED" })
    );
    assert!(
        !resolved
            .compilation_report()
            .warnings
            .iter()
            .any(|entry| { entry.code == "KF-MODEL-OPTIONAL-FALLBACK" })
    );
}

#[test]
fn required_model_must_satisfy_every_declared_constraint() {
    let mut input = resolution_fixture();
    input
        .components
        .components
        .get_mut(&RegistrySymbol::new("models.anthropic.sonnet").unwrap())
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .max_context_tokens = std::num::NonZeroU32::new(1024).unwrap();

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-RUNTIME-001");
}

#[test]
fn required_model_rejects_modality_tool_structured_output_and_residency_misses() {
    let symbol = RegistrySymbol::new("models.anthropic.sonnet").unwrap();
    let mut modality = resolution_fixture();
    modality
        .components
        .components
        .get_mut(&symbol)
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .modalities
        .clear();
    let mut tool_calling = resolution_fixture();
    tool_calling
        .components
        .components
        .get_mut(&symbol)
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .tool_calling = false;
    let mut structured_output = resolution_fixture();
    structured_output
        .components
        .components
        .get_mut(&symbol)
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .structured_output = false;
    let mut residency = resolution_fixture();
    residency
        .components
        .components
        .get_mut(&symbol)
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .residency = ResidencyClass::new("regional").unwrap();

    for input in [modality, tool_calling, structured_output, residency] {
        assert_eq!(
            resolve_agent(input).unwrap_err()[0].code.as_str(),
            "KF-RUNTIME-001"
        );
    }
}

#[test]
fn interactive_model_requirement_rejects_batch_only_metadata() {
    let mut input = resolution_fixture();
    input
        .components
        .components
        .get_mut(&RegistrySymbol::new("models.anthropic.sonnet").unwrap())
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .latency_class = ModelLatencyClass::Batch;

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-RUNTIME-001");
}

#[test]
fn component_symbols_are_checked_against_their_exact_kinds() {
    let mut input = resolution_fixture();
    input
        .components
        .components
        .get_mut(&RegistrySymbol::new("capability-providers.primary").unwrap())
        .unwrap()
        .kind = ComponentKind::AuditSink;

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-RUNTIME-001");
}

#[test]
fn harness_profile_symbol_is_checked_against_its_exact_kind() {
    let mut input = resolution_fixture();
    input
        .components
        .components
        .get_mut(&RegistrySymbol::new("profiles.deepagents").unwrap())
        .unwrap()
        .kind = ComponentKind::AuditSink;

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-RUNTIME-001");
}

#[test]
fn binding_cannot_enable_content_capture_the_package_disallows() {
    let package = package(
        &root_manifest("kiteframe.capability.point-of-use-auth@1", false),
        None,
    );
    let lock = lock_package(&package, &empty_catalog(), CandidatePolicy::AllowAll).unwrap();
    let mut input = resolution_fixture();
    input.package = package;
    input.lock = lock;
    input.binding.spec.content_capture = Some(binding_capture());

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn content_capture_is_the_portable_and_binding_intersection() {
    let mut input = resolution_fixture();
    input.binding.spec.content_capture = Some(binding_capture());

    let resolved = resolve_agent(input).unwrap();

    assert!(resolved.content_capture().allowed);
    assert_eq!(
        resolved.content_capture().classifications,
        vec![DataClassification::Confidential]
    );
}

#[test]
fn child_resolution_requires_its_own_exact_lock() {
    let package = package(parent_with_child_manifest(), Some(child_manifest()));
    let lock = lock_package(&package, &empty_catalog(), CandidatePolicy::AllowAll).unwrap();
    let mut input = resolution_fixture();
    input.package = package;
    input.lock = lock;
    input.child_locks.clear();

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-LOCK-001");
}

#[test]
fn declared_child_carries_parent_delegation_without_expansion() {
    let package = package(parent_with_child_manifest(), Some(child_manifest()));
    let child = package.subagents().values().next().unwrap();
    let child_lock = lock_package(child, &empty_catalog(), CandidatePolicy::AllowAll).unwrap();
    let lock = lock_package(&package, &empty_catalog(), CandidatePolicy::AllowAll).unwrap();
    let mut input = resolution_fixture();
    input
        .child_locks
        .insert(child.manifest().metadata.clone(), child_lock);
    input.package = package;
    input.lock = lock;

    let resolved = resolve_agent(input).unwrap();

    assert_eq!(resolved.subagents().len(), 1);
    assert_eq!(
        resolved.subagents()[0].delegation.agent.as_str(),
        "agents/child/agent.yaml"
    );
    assert!(resolved.subagents()[0].delegation.capabilities.is_empty());
}

#[test]
fn target_and_component_catalog_must_match_the_selected_binding() {
    let mut input = resolution_fixture();
    input.components.target = serde_json::from_value(json!("other-runtime")).unwrap();

    let errors = resolve_agent(input).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-RUNTIME-001");
}
