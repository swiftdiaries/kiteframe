use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    ApprovalRequirement, CapabilityCatalog, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityIdentity, CapabilityName, CapabilityReleaseVersion, CatalogIdentity,
    ComponentMetadataCatalog, ConfirmationRequirement, ConsentRequirement, EffectClassification,
    ExecutionMode, FreshnessRequirement, IdempotencyRequirement, NonEmptySet, PackageIdentity,
    ResourceSelectorSchema, RuntimeTargetDescriptor, Sha256Digest,
};
use kiteframe_core::{AgentPackage, PackageLimits, load_package};
use kiteframe_resolver::{
    CandidatePolicy, ResolutionInput, lock_package, resolve_agent, validate_catalog,
};
use proptest::prelude::*;
use serde_json::json;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn components() -> ComponentMetadataCatalog {
    serde_json::from_slice(
        &fs::read(fixture("components/deepagents-test.json")).expect("component fixture"),
    )
    .unwrap()
}

fn binding() -> kiteframe_contract::RuntimeBinding {
    serde_json::from_value(json!({
        "apiVersion": "kiteframe.dev/binding/v1alpha1",
        "kind": "RuntimeBinding",
        "metadata": {"runtime": "deepagents"},
        "spec": {
            "models": {"primary": "models.anthropic.sonnet"},
            "capabilityProvider": "capability-providers.primary",
            "auditSink": "audit-sinks.ledger"
        }
    }))
    .unwrap()
}

fn target() -> RuntimeTargetDescriptor {
    RuntimeTargetDescriptor {
        target: serde_json::from_value(json!("deepagents")).unwrap(),
        supported_features: BTreeSet::new(),
        target_digest: Sha256Digest::from_bytes([7; Sha256Digest::BYTE_LENGTH]),
    }
}

fn load_test_package(manifest: &str, child_manifest: Option<&str>) -> AgentPackage {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir_all(directory.path().join("prompts")).unwrap();
    fs::write(directory.path().join("agent.yaml"), manifest).unwrap();
    fs::write(directory.path().join("prompts/system.md"), "System prompt").unwrap();
    if let Some(child_manifest) = child_manifest {
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

fn capability_manifest(required: bool, resources: &[&str]) -> String {
    let resources = if resources.is_empty() {
        "      resources: []".to_owned()
    } else {
        format!(
            "      resources:\n{}",
            resources
                .iter()
                .map(|resource| format!("        - {resource}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: {{ name: support, version: 0.1.0 }}
spec:
  prompt: {{ system: prompts/system.md }}
  models:
    primary: {{ capabilities: [text] }}
  capabilities:
    - name: cases.read
      version: "^1.0"
      required: {required}
{resources}
"#
    )
}

fn parent_manifest(capabilities: &[&str]) -> String {
    let capabilities = capabilities.join(", ");
    format!(
        r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: {{ name: parent, version: 0.1.0 }}
spec:
  prompt: {{ system: prompts/system.md }}
  models:
    primary: {{ capabilities: [text] }}
  delegation:
    - agent: agents/child/agent.yaml
      capabilities: [{capabilities}]
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

fn descriptor(max_admission_age_seconds: u64) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: CapabilityIdentity::try_new(
            CapabilityName::new("cases.read").unwrap(),
            CapabilityReleaseVersion::new("1.0.0").unwrap(),
        )
        .unwrap(),
        summary: "Read a case".to_owned(),
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object"
        }),
        output_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object"
        }),
        stable_errors: Vec::new(),
        execution_modes: NonEmptySet::try_new(BTreeSet::from([ExecutionMode::Immediate])).unwrap(),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect: EffectClassification::ReadOnly,
        idempotency: IdempotencyRequirement::None,
        freshness: FreshnessRequirement {
            max_admission_age_seconds: NonZeroU64::new(max_admission_age_seconds),
            policy_revision_required: true,
            max_input_age_seconds: None,
        },
        preconditions: Vec::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn catalog(descriptors: Vec<CapabilityDescriptor>) -> kiteframe_resolver::ValidatedCatalog {
    let catalog = CapabilityCatalog::try_new(
        CatalogIdentity {
            name: "support".to_owned(),
            revision: "v1".to_owned(),
        },
        descriptors,
    )
    .unwrap();
    validate_catalog(&serde_json::to_vec(&catalog).unwrap()).unwrap()
}

fn input(
    package: AgentPackage,
    lock: kiteframe_contract::CapabilityLock,
    child_locks: BTreeMap<PackageIdentity, kiteframe_contract::CapabilityLock>,
) -> ResolutionInput {
    ResolutionInput {
        package,
        lock,
        child_locks,
        binding: binding(),
        target: target(),
        components: components(),
    }
}

fn capability_resources(resolved: &kiteframe_contract::ResolvedAgent) -> BTreeSet<String> {
    resolved.capability_requirements()[0]
        .resources
        .iter()
        .cloned()
        .collect()
}

fn resolve_parent(capabilities: &[&str]) -> kiteframe_contract::ResolvedAgent {
    let package = load_test_package(&parent_manifest(capabilities), Some(child_manifest()));
    let empty_catalog = catalog(Vec::new());
    let root_lock = lock_package(&package, &empty_catalog, CandidatePolicy::AllowAll).unwrap();
    let child = package.subagents().values().next().unwrap();
    let child_lock = lock_package(child, &empty_catalog, CandidatePolicy::AllowAll).unwrap();
    let child_locks = BTreeMap::from([(child.manifest().metadata.clone(), child_lock)]);
    resolve_agent(input(package, root_lock, child_locks)).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn removing_a_capability_candidate_never_increases_the_resolved_envelope(
        max_age in 1_u64..600,
    ) {
        let package = load_test_package(&capability_manifest(false, &["tenant:a"]), None);
        let available = catalog(vec![descriptor(max_age)]);
        let broad_lock =
            lock_package(&package, &available, CandidatePolicy::AllowAll).unwrap();
        let narrow_lock = lock_package(
            &package,
            &available,
            CandidatePolicy::exact(Vec::<String>::new()),
        )
        .unwrap();

        let broad = resolve_agent(input(package.clone(), broad_lock, BTreeMap::new())).unwrap();
        let narrow = resolve_agent(input(package, narrow_lock, BTreeMap::new())).unwrap();

        prop_assert_eq!(broad.capability_requirements().len(), 1);
        prop_assert!(narrow.capability_requirements().is_empty());
    }

    #[test]
    fn removing_resource_selectors_never_increases_actual_resolved_resources(
        keep in 0_usize..4,
    ) {
        let broad_values = ["tenant:a", "tenant:b", "tenant:c"];
        let narrow_values = &broad_values[..keep.min(broad_values.len())];
        let available = catalog(vec![descriptor(300)]);
        let broad_package =
            load_test_package(&capability_manifest(true, &broad_values), None);
        let narrow_package =
            load_test_package(&capability_manifest(true, narrow_values), None);
        let broad_lock =
            lock_package(&broad_package, &available, CandidatePolicy::AllowAll).unwrap();
        let narrow_lock =
            lock_package(&narrow_package, &available, CandidatePolicy::AllowAll).unwrap();

        let broad =
            resolve_agent(input(broad_package, broad_lock, BTreeMap::new())).unwrap();
        let narrow =
            resolve_agent(input(narrow_package, narrow_lock, BTreeMap::new())).unwrap();
        let broad_resources = capability_resources(&broad);
        let narrow_resources = capability_resources(&narrow);

        prop_assert!(narrow_resources.is_subset(&broad_resources));
        prop_assert_eq!(narrow_resources.len(), narrow_values.len());
    }

    #[test]
    fn narrowing_delegation_never_increases_actual_resolved_child_authority(
        keep in 0_usize..4,
    ) {
        let broad_values = ["cases.read", "cases.comment", "cases.close"];
        let narrow_values = &broad_values[..keep.min(broad_values.len())];

        let broad = resolve_parent(&broad_values);
        let narrow = resolve_parent(narrow_values);
        let broad_capabilities = &broad.subagents()[0].delegation.capabilities;
        let narrow_capabilities = &narrow.subagents()[0].delegation.capabilities;

        prop_assert!(narrow_capabilities.is_subset(broad_capabilities));
        prop_assert_eq!(narrow_capabilities.len(), narrow_values.len());
    }

    #[test]
    fn exact_lock_digest_preserves_narrower_freshness_material_through_resolution(
        short in 1_u64..300,
        extension in 1_u64..300,
    ) {
        let long = short + extension;
        let package = load_test_package(&capability_manifest(true, &["tenant:a"]), None);
        let broad_lock = lock_package(
            &package,
            &catalog(vec![descriptor(long)]),
            CandidatePolicy::AllowAll,
        )
        .unwrap();
        let narrow_lock = lock_package(
            &package,
            &catalog(vec![descriptor(short)]),
            CandidatePolicy::AllowAll,
        )
        .unwrap();
        let broad_freshness =
            serde_json::to_value(&broad_lock.capabilities[0].descriptor).unwrap()["freshness"]
                .clone();
        let narrow_freshness =
            serde_json::to_value(&narrow_lock.capabilities[0].descriptor).unwrap()["freshness"]
                .clone();

        let broad = resolve_agent(input(
            package.clone(),
            broad_lock.clone(),
            BTreeMap::new(),
        ))
        .unwrap();
        let narrow =
            resolve_agent(input(package, narrow_lock.clone(), BTreeMap::new())).unwrap();

        prop_assert_eq!(broad.lock_digest(), &broad_lock.lock_digest);
        prop_assert_eq!(narrow.lock_digest(), &narrow_lock.lock_digest);
        prop_assert_ne!(broad.lock_digest(), narrow.lock_digest());
        prop_assert_eq!(
            broad_freshness["maxAdmissionAgeSeconds"].as_u64(),
            Some(long)
        );
        prop_assert_eq!(
            narrow_freshness["maxAdmissionAgeSeconds"].as_u64(),
            Some(short)
        );
        prop_assert_eq!(
            broad.capability_requirements(),
            narrow.capability_requirements()
        );
    }
}
