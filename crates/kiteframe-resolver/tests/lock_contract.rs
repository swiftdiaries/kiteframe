use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    ApprovalRequirement, CapabilityCatalog, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityIdentity, CapabilityName, CapabilityReleaseVersion, CatalogIdentity,
    ConfirmationRequirement, ConsentRequirement, EffectClassification, EvidenceRequirement,
    ExecutionMode, FreshnessRequirement, IdempotencyRequirement, NonEmptySet,
    ResourceSelectorSchema, Sha256Digest,
};
use kiteframe_core::{PackageLimits, load_package};
use kiteframe_resolver::{
    CandidatePolicy, lock_package, validate_catalog, verify_lock, write_lock_atomic,
};
use serde_json::json;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/resolution")
        .join(name)
}

fn support_package() -> kiteframe_core::AgentPackage {
    load_package(fixture("stale-package").as_path(), PackageLimits::V1).unwrap()
}

fn descriptor(name: &str, version: &str, approval: ApprovalRequirement) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: CapabilityIdentity::try_new(
            CapabilityName::new(name).unwrap(),
            CapabilityReleaseVersion::new(version).unwrap(),
        )
        .unwrap(),
        summary: "Read a support case".to_owned(),
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
        freshness: FreshnessRequirement::default(),
        preconditions: Vec::new(),
        confirmation: ConfirmationRequirement::None,
        approval,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn catalog_with(descriptors: Vec<CapabilityDescriptor>) -> kiteframe_resolver::ValidatedCatalog {
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

fn support_catalog() -> kiteframe_resolver::ValidatedCatalog {
    catalog_with(vec![descriptor(
        "cases.read",
        "1.2.0",
        ApprovalRequirement::None,
    )])
}

fn support_lock() -> kiteframe_contract::CapabilityLock {
    lock_package(
        &support_package(),
        &support_catalog(),
        CandidatePolicy::AllowAll,
    )
    .unwrap()
}

#[test]
fn locked_compile_never_substitutes_another_compatible_version() {
    let package = support_package();
    let lock = support_lock();
    let catalog = catalog_with(vec![descriptor(
        "cases.read",
        "1.3.0",
        ApprovalRequirement::None,
    )]);

    let errors = verify_lock(&package, &lock, Some(&catalog)).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-LOCK-001");
}

#[test]
fn changed_safety_metadata_is_tampering() {
    let package = support_package();
    let lock = support_lock();
    let catalog = catalog_with(vec![descriptor(
        "cases.read",
        "1.2.0",
        ApprovalRequirement::Required {
            evidence: EvidenceRequirement {
                kind: "case-approval".to_owned(),
                issuer: None,
            },
        },
    )]);

    let errors = verify_lock(&package, &lock, Some(&catalog)).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-LOCK-002");
}

#[test]
fn offline_verification_rejects_each_embedded_descriptor_digest_change() {
    let package = support_package();
    let lock = support_lock();

    for changed in [
        {
            let mut lock = lock.clone();
            lock.capabilities[0].input_schema_digest = Sha256Digest::from_bytes([1; 32]);
            lock
        },
        {
            let mut lock = lock.clone();
            lock.capabilities[0].output_schema_digest = Sha256Digest::from_bytes([2; 32]);
            lock
        },
        {
            let mut lock = lock.clone();
            lock.capabilities[0].stable_error_set_digest = Sha256Digest::from_bytes([3; 32]);
            lock
        },
        {
            let mut lock = lock.clone();
            lock.capabilities[0].safety_metadata_digest = Sha256Digest::from_bytes([4; 32]);
            lock
        },
    ] {
        let errors = verify_lock(&package, &changed, None).unwrap_err();
        assert_eq!(errors[0].code.as_str(), "KF-LOCK-002");
    }
}

#[test]
fn offline_verification_rejects_changed_package_and_unsupported_resolver() {
    let stale_package =
        load_package(fixture("tampered-descriptor").as_path(), PackageLimits::V1).unwrap();
    let lock = support_lock();
    let errors = verify_lock(&stale_package, &lock, None).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-LOCK-001");

    let mut incompatible = lock;
    incompatible.resolver_version = "999.0.0".to_owned();
    let errors = verify_lock(&support_package(), &incompatible, None).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-LOCK-001");
}

#[test]
fn atomic_write_replaces_complete_lock_and_preserves_existing_file_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("capability.lock");
    let lock = support_lock();
    write_lock_atomic(&path, &lock).unwrap();
    let baseline = std::fs::read(&path).unwrap();

    let replacement = lock_package(
        &support_package(),
        &catalog_with(vec![descriptor(
            "cases.read",
            "1.3.0",
            ApprovalRequirement::None,
        )]),
        CandidatePolicy::AllowAll,
    )
    .unwrap();
    write_lock_atomic(&path, &replacement).unwrap();
    let replacement_bytes = std::fs::read(&path).unwrap();
    assert_ne!(replacement_bytes, baseline);

    let errors = lock_package(
        &support_package(),
        &catalog_with(vec![descriptor(
            "other.read",
            "1.0.0",
            ApprovalRequirement::None,
        )]),
        CandidatePolicy::AllowAll,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-CAT-001");
    assert_eq!(std::fs::read(&path).unwrap(), replacement_bytes);
}

#[test]
fn support_lock_fixture_is_the_canonical_atomic_lock_output() {
    let expected = include_str!("../../../tests/fixtures/locks/support-agent.lock");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("capability.lock");
    let lock = support_lock();

    write_lock_atomic(&path, &lock).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected.trim_end());
    let fixture_lock: kiteframe_contract::CapabilityLock = serde_json::from_str(expected).unwrap();
    verify_lock(&support_package(), &fixture_lock, None).unwrap();
    verify_lock(&support_package(), &lock, None).unwrap();
}
