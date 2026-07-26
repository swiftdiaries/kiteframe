use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    ApprovalRequirement, CapabilityCatalog, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityIdentity, CapabilityName, CapabilityReleaseVersion, CatalogIdentity,
    ConfirmationRequirement, ConsentRequirement, EffectClassification, EvidenceRequirement,
    ExecutionMode, FreshnessRequirement, IdempotencyRequirement, LockSchemaVersion, NonEmptySet,
    ResourceSelectorSchema, Sha256Digest,
};
use kiteframe_core::{PackageLimits, load_package};
use kiteframe_resolver::{
    CandidatePolicy, lock_package, validate_catalog, verify_lock, write_lock_atomic,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/resolution")
        .join(name)
}

fn support_package() -> kiteframe_core::AgentPackage {
    load_package(fixture("stale-package").as_path(), PackageLimits::V1).unwrap()
}

fn ordered_package() -> kiteframe_core::AgentPackage {
    load_package(fixture("ordered-package").as_path(), PackageLimits::V1).unwrap()
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

fn catalog_with_identity(
    name: &str,
    revision: &str,
    descriptors: Vec<CapabilityDescriptor>,
) -> kiteframe_resolver::ValidatedCatalog {
    let catalog = CapabilityCatalog::try_new(
        CatalogIdentity {
            name: name.to_owned(),
            revision: revision.to_owned(),
        },
        descriptors,
    )
    .unwrap();
    validate_catalog(&serde_json::to_vec(&catalog).unwrap()).unwrap()
}

fn catalog_with(descriptors: Vec<CapabilityDescriptor>) -> kiteframe_resolver::ValidatedCatalog {
    catalog_with_identity("support", "v1", descriptors)
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

fn ordered_lock() -> kiteframe_contract::CapabilityLock {
    lock_package(
        &ordered_package(),
        &catalog_with(vec![
            descriptor("cases.read", "1.2.0", ApprovalRequirement::None),
            descriptor("cases.comment", "1.0.0", ApprovalRequirement::None),
        ]),
        CandidatePolicy::AllowAll,
    )
    .unwrap()
}

fn refresh_lock_digest(lock: &mut kiteframe_contract::CapabilityLock) {
    let mut material = serde_json::to_value(&*lock).unwrap();
    material.as_object_mut().unwrap().remove("lockDigest");
    lock.lock_digest = Sha256Digest::from_bytes(
        Sha256::digest(serde_json_canonicalizer::to_vec(&material).unwrap()).into(),
    );
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

    assert!(
        errors
            .iter()
            .any(|error| error.code.as_str() == "KF-LOCK-002")
    );
}

#[test]
fn verification_preserves_each_distinct_failure_in_stable_order() {
    let mut lock = support_lock();
    lock.package_portable_digest = Sha256Digest::from_bytes([7; 32]);
    lock.resolver_version = "999.0.0".to_owned();

    let errors = verify_lock(&support_package(), &lock, None).unwrap_err();

    assert_eq!(errors.len(), 3);
    assert_eq!(
        errors[0].message.as_str(),
        "capability lock resolver version is unsupported"
    );
    assert_eq!(
        errors[1].message.as_str(),
        "package portable digest does not match capability lock"
    );
    assert_eq!(errors[2].code.as_str(), "KF-LOCK-002");
}

#[test]
fn catalog_descriptor_drift_does_not_hide_catalog_digest_drift() {
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

    let errors = verify_lock(&support_package(), &lock, Some(&catalog)).unwrap_err();
    let messages: Vec<_> = errors.iter().map(|error| error.message.as_str()).collect();

    assert!(messages.contains(&"capability catalog descriptor does not match locked descriptor"));
    assert!(messages.contains(&"capability catalog digest does not match capability lock"));
}

#[test]
fn catalog_identity_revision_and_digest_drift_are_all_reported() {
    let lock = support_lock();
    let catalog = catalog_with_identity(
        "other-support",
        "v2",
        vec![descriptor("cases.read", "1.2.0", ApprovalRequirement::None)],
    );

    let errors = verify_lock(&support_package(), &lock, Some(&catalog)).unwrap_err();
    let messages: Vec<_> = errors.iter().map(|error| error.message.as_str()).collect();

    assert!(messages.contains(&"capability catalog identity does not match capability lock"));
    assert!(messages.contains(&"capability catalog revision does not match capability lock"));
    assert!(messages.contains(&"capability catalog digest does not match capability lock"));
}

#[test]
fn canonical_lock_order_and_identity_uniqueness_are_required() {
    let mut reordered = ordered_lock();
    reordered.capabilities.swap(0, 1);
    refresh_lock_digest(&mut reordered);
    let errors = verify_lock(&ordered_package(), &reordered, None).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.message.as_str() == "locked capabilities are not sorted by identity"
    }));

    let mut duplicate = ordered_lock();
    duplicate
        .capabilities
        .insert(1, duplicate.capabilities[0].clone());
    refresh_lock_digest(&mut duplicate);
    let errors = verify_lock(&ordered_package(), &duplicate, None).unwrap_err();
    assert!(errors.iter().any(|error| {
        error.message.as_str() == "locked capabilities contain duplicate identity"
    }));
}

#[test]
fn unsupported_schema_and_self_digest_are_rejected() {
    let mut unsupported = support_lock();
    unsupported.schema_version = LockSchemaVersion::Unsupported;
    refresh_lock_digest(&mut unsupported);
    let errors = verify_lock(&support_package(), &unsupported, None).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-LOCK-001");

    let mut tampered = support_lock();
    tampered.lock_digest = Sha256Digest::from_bytes([8; 32]);
    let errors = verify_lock(&support_package(), &tampered, None).unwrap_err();
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
fn direct_atomic_write_rejection_preserves_existing_output() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("capability.lock");
    write_lock_atomic(&path, &support_lock()).unwrap();
    let baseline = std::fs::read(&path).unwrap();
    let mut invalid = ordered_lock();
    invalid.capabilities.swap(0, 1);
    refresh_lock_digest(&mut invalid);

    let error = write_lock_atomic(&path, &invalid).unwrap_err();

    assert_eq!(error.code.as_str(), "KF-LOCK-002");
    assert_eq!(std::fs::read(&path).unwrap(), baseline);
}

#[test]
fn support_lock_fixture_is_the_canonical_atomic_lock_output() {
    let expected = include_str!("../../../tests/fixtures/locks/support-agent.lock");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("capability.lock");
    let lock = support_lock();

    assert!(!expected.ends_with('\n'));
    write_lock_atomic(&path, &lock).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    let fixture_lock: kiteframe_contract::CapabilityLock = serde_json::from_str(expected).unwrap();
    verify_lock(&support_package(), &fixture_lock, None).unwrap();
    verify_lock(&support_package(), &lock, None).unwrap();
}
