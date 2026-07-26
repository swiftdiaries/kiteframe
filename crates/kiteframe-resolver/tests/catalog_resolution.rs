use std::collections::{BTreeSet, HashSet};

use kiteframe_contract::{
    ApprovalRequirement, CapabilityCatalog, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityErrorDescriptor, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CapabilityRequirement, CapabilityVersion, CatalogIdentity, ConfirmationRequirement,
    ConsentRequirement, EffectClassification, EvidenceRequirement, ExecutionMode,
    FreshnessRequirement, IdempotencyRequirement, NonEmptySet, ResourceSelectorSchema,
};
use kiteframe_resolver::{CandidatePolicy, select_capabilities_with_warnings, validate_catalog};
use proptest::prelude::*;
use serde_json::json;

fn descriptor_parts(name: &str, version: &str) -> CapabilityDescriptorParts {
    CapabilityDescriptorParts {
        identity: CapabilityIdentity::try_new(
            CapabilityName::new(name).unwrap(),
            CapabilityReleaseVersion::new(version).unwrap(),
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
        freshness: FreshnessRequirement::default(),
        preconditions: Vec::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    }
}

fn descriptor(name: &str, version: &str) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(descriptor_parts(name, version)).unwrap()
}

fn catalog_with_descriptors(descriptors: Vec<CapabilityDescriptor>) -> Vec<u8> {
    serde_json::to_vec(
        &CapabilityCatalog::try_new(
            CatalogIdentity {
                name: "support".to_owned(),
                revision: "v1".to_owned(),
            },
            descriptors,
        )
        .unwrap(),
    )
    .unwrap()
}

fn catalog_with_versions(name: &str, versions: impl IntoIterator<Item = &'static str>) -> Vec<u8> {
    let descriptors = versions
        .into_iter()
        .map(|version| descriptor(name, version))
        .collect();
    catalog_with_descriptors(descriptors)
}

fn requirement(name: &str, version: &str, required: bool) -> CapabilityRequirement {
    CapabilityRequirement {
        name: CapabilityName::new(name).unwrap(),
        version: CapabilityVersion::new(version).unwrap(),
        required,
        resources: BTreeSet::new(),
    }
}

#[test]
fn selects_highest_compatible_version() {
    let catalog = validate_catalog(&catalog_with_versions(
        "cases.read",
        ["1.2.0", "1.9.3", "2.0.0"],
    ))
    .unwrap();
    let selected = select_capabilities_with_warnings(
        &[requirement("cases.read", "^1.2", true)],
        &catalog,
        CandidatePolicy::AllowAll,
    )
    .unwrap();

    assert_eq!(
        selected.selected()[0]
            .descriptor()
            .identity()
            .version()
            .to_string(),
        "1.9.3"
    );
}

#[test]
fn policy_can_only_remove_candidates() {
    let catalog =
        validate_catalog(&catalog_with_versions("cases.read", ["1.2.0", "1.9.3"])).unwrap();
    let selected = select_capabilities_with_warnings(
        &[requirement("cases.read", "^1.2", true)],
        &catalog,
        CandidatePolicy::exact(["cases.read@1.2.0"]),
    )
    .unwrap();

    assert_eq!(
        selected.selected()[0]
            .descriptor()
            .identity()
            .version()
            .to_string(),
        "1.2.0"
    );
}

#[test]
fn reordered_catalog_bytes_select_identically_after_canonical_validation() {
    let bytes = catalog_with_versions("cases.read", ["1.2.0", "1.9.3", "2.0.0"]);
    let mut reordered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    reordered["descriptors"].as_array_mut().unwrap().reverse();
    let reordered = serde_json::to_vec(&reordered).unwrap();

    let a = validate_catalog(&bytes).unwrap();
    let b = validate_catalog(&reordered).unwrap();
    assert_eq!(a.catalog_digest(), b.catalog_digest());
}

#[test]
fn required_miss_uses_the_catalog_incompatible_diagnostic() {
    let catalog = validate_catalog(&catalog_with_versions("cases.read", ["1.2.0"])).unwrap();
    let diagnostics = select_capabilities_with_warnings(
        &[requirement("cases.write", "^1.0", true)],
        &catalog,
        CandidatePolicy::AllowAll,
    )
    .unwrap_err();

    assert_eq!(diagnostics[0].code.as_str(), "KF-CAT-001");
}

#[test]
fn distinct_required_misses_produce_distinct_stably_ordered_diagnostics() {
    let catalog = validate_catalog(&catalog_with_versions("cases.comment", ["1.2.0"])).unwrap();
    let diagnostics = select_capabilities_with_warnings(
        &[
            requirement("cases.write", "^1.0", true),
            requirement("cases.read", "^1.0", true),
        ],
        &catalog,
        CandidatePolicy::AllowAll,
    )
    .unwrap_err();

    assert_eq!(diagnostics.len(), 2);
    assert_eq!(
        diagnostics[0].message.as_str(),
        "no compatible catalog capability for cases.read ^1.0"
    );
    assert_eq!(
        diagnostics[1].message.as_str(),
        "no compatible catalog capability for cases.write ^1.0"
    );
}

#[test]
fn optional_miss_is_retained_as_a_stable_warning() {
    let catalog = validate_catalog(&catalog_with_versions("cases.read", ["1.2.0"])).unwrap();
    let outcome = select_capabilities_with_warnings(
        &[requirement("cases.write", "^1.0", false)],
        &catalog,
        CandidatePolicy::AllowAll,
    )
    .unwrap();

    assert!(outcome.selected().is_empty());
    assert_eq!(outcome.warnings()[0].code, "KF-CAT-001");
}

#[test]
fn support_fixture_is_a_validated_catalog() {
    let catalog = validate_catalog(include_bytes!(
        "../../../tests/fixtures/catalogs/support-v1.json"
    ))
    .unwrap();

    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(
        catalog.descriptors()[0].identity().name().as_str(),
        "cases.read"
    );
}

#[test]
fn validated_descriptor_exposes_independent_canonical_part_digests() {
    let baseline = validate_catalog(&catalog_with_descriptors(vec![descriptor(
        "cases.read",
        "1.2.0",
    )]))
    .unwrap()
    .validated_descriptors()[0]
        .clone();

    let mut changed_input = descriptor_parts("cases.read", "1.2.0");
    changed_input.input_schema = json!({"type": "string"});
    let changed_input = validate_catalog(&catalog_with_descriptors(vec![
        CapabilityDescriptor::try_new(changed_input).unwrap(),
    ]))
    .unwrap()
    .validated_descriptors()[0]
        .clone();

    let mut changed_output = descriptor_parts("cases.read", "1.2.0");
    changed_output.output_schema = json!({"type": "string"});
    let changed_output = validate_catalog(&catalog_with_descriptors(vec![
        CapabilityDescriptor::try_new(changed_output).unwrap(),
    ]))
    .unwrap()
    .validated_descriptors()[0]
        .clone();

    let mut changed_errors = descriptor_parts("cases.read", "1.2.0");
    changed_errors.stable_errors = vec![
        CapabilityErrorDescriptor::try_new("not_found", "case", "never", "Case not found").unwrap(),
    ];
    let changed_errors = validate_catalog(&catalog_with_descriptors(vec![
        CapabilityDescriptor::try_new(changed_errors).unwrap(),
    ]))
    .unwrap()
    .validated_descriptors()[0]
        .clone();

    let mut changed_safety = descriptor_parts("cases.read", "1.2.0");
    changed_safety.approval = ApprovalRequirement::Required {
        evidence: EvidenceRequirement {
            kind: "case-approval".to_owned(),
            issuer: None,
        },
    };
    let changed_safety = validate_catalog(&catalog_with_descriptors(vec![
        CapabilityDescriptor::try_new(changed_safety).unwrap(),
    ]))
    .unwrap()
    .validated_descriptors()[0]
        .clone();

    assert_ne!(
        baseline.input_schema_digest(),
        changed_input.input_schema_digest()
    );
    assert_ne!(
        baseline.output_schema_digest(),
        changed_output.output_schema_digest()
    );
    assert_ne!(
        baseline.stable_error_set_digest(),
        changed_errors.stable_error_set_digest()
    );
    assert_ne!(
        baseline.safety_metadata_digest(),
        changed_safety.safety_metadata_digest()
    );
}

proptest! {
    #[test]
    fn catalog_order_never_changes_selection(order in prop::collection::vec(0_usize..3, 3)) {
        prop_assume!(HashSet::<usize>::from_iter(order.iter().copied()).len() == 3);
        let versions = ["1.2.0", "1.9.3", "2.0.0"];
        let ordered = order.into_iter().map(|index| versions[index]).collect::<Vec<_>>();
        let catalog = validate_catalog(&catalog_with_versions("cases.read", ordered)).unwrap();
        let selected = select_capabilities_with_warnings(
            &[requirement("cases.read", "^1.2", true)],
            &catalog,
            CandidatePolicy::AllowAll,
        )
        .unwrap();
        prop_assert_eq!(selected.selected()[0].descriptor().identity().version().to_string(), "1.9.3");
    }
}
