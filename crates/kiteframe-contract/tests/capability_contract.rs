use std::collections::BTreeSet;

use kiteframe_contract::{
    ApprovalRequirement, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityErrorDescriptor, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    ConfirmationRequirement, ConsentRequirement, EffectClassification, ExecutionMode,
    FreshnessRequirement, IdempotencyRequirement, NonEmptySet, PreconditionDescriptor,
    ResolvedAgent, ResourceSelectorSchema,
};
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
        stable_errors: vec![
            CapabilityErrorDescriptor::try_new("not_found", "case", "never", "Case not found")
                .unwrap(),
        ],
        execution_modes: NonEmptySet::try_new(BTreeSet::from([ExecutionMode::Immediate])).unwrap(),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect: EffectClassification::ReadOnly,
        idempotency: IdempotencyRequirement::None,
        freshness: FreshnessRequirement::default(),
        preconditions: Vec::<PreconditionDescriptor>::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    }
}

#[test]
fn effectful_descriptor_requires_idempotency() {
    let mut parts = descriptor_parts("cases.comment", "1.0.0");
    parts.effect = EffectClassification::ExternalSideEffect;
    parts.idempotency = IdempotencyRequirement::None;
    let errors = CapabilityDescriptor::try_new(parts).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("idempotency"));
}

#[test]
fn remote_schema_reference_is_rejected() {
    let mut parts = descriptor_parts("cases.read", "1.2.0");
    parts.input_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "https://example.invalid/case.json"
    });
    assert!(CapabilityDescriptor::try_new(parts).is_err());
}

#[test]
fn resolved_agent_schema_contains_no_credentials_or_runtime_objects() {
    let schema = serde_json::to_string(&schemars::schema_for!(ResolvedAgent)).unwrap();
    assert!(!schema.contains("credential"));
    assert!(!schema.contains("endpoint"));
    assert!(!schema.contains("runtimeObject"));
}
