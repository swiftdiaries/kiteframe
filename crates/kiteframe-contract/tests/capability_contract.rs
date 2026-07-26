use std::collections::{BTreeMap, BTreeSet};

use kiteframe_contract::{
    AgentName, ApprovalRequirement, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityErrorDescriptor, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CompilationDecision, CompilationReport, CompilationWarning, ConfirmationRequirement,
    ConsentRequirement, DelegationRequirement, EffectClassification, ExecutionMode, FeatureId,
    FreshnessRequirement, IdempotencyRequirement, IrSchemaVersion, JsonSchema2020_12, NonEmptySet,
    PackageIdentity, PackagePath, PackageVersion, PreconditionDescriptor, ResolvedAgent,
    ResolvedAgentParts, ResolvedCapabilityRequirement, ResolvedContentCaptureRequirement,
    ResolvedSubagent, ResourceSelectorSchema, Sha256Digest,
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

#[test]
fn schemas_reject_remote_dynamic_references_and_invalid_draft_keywords() {
    for schema in [
        json!({"$dynamicRef": "https://example.invalid/case.json"}),
        json!({"type": 42}),
        json!({"$ref": "#/$defs/missing"}),
    ] {
        assert!(JsonSchema2020_12::try_new(schema).is_err());
    }
}

#[test]
fn execution_modes_reject_an_empty_wire_set() {
    assert!(serde_json::from_value::<NonEmptySet<ExecutionMode>>(json!([])).is_err());
}

#[test]
fn feature_major_must_fit_the_public_numeric_accessor() {
    assert!(FeatureId::new("kiteframe.capability.deferred@18446744073709551616").is_err());
}

#[test]
fn resolved_digest_is_independent_of_nested_collection_order() {
    let first = ResolvedAgent::try_new(resolved_parts(
        vec!["team:z", "team:a"],
        vec![subagent("child", "2.0.0"), subagent("child", "1.0.0")],
        CompilationReport {
            warnings: vec![
                CompilationWarning {
                    code: "W2".into(),
                    message: "second".into(),
                },
                CompilationWarning {
                    code: "W1".into(),
                    message: "first".into(),
                },
            ],
            decisions: vec![
                CompilationDecision {
                    subject: "models".into(),
                    outcome: "selected".into(),
                },
                CompilationDecision {
                    subject: "features".into(),
                    outcome: "enabled".into(),
                },
            ],
        },
    ))
    .unwrap();
    let second = ResolvedAgent::try_new(resolved_parts(
        vec!["team:a", "team:z"],
        vec![subagent("child", "1.0.0"), subagent("child", "2.0.0")],
        CompilationReport {
            warnings: vec![
                CompilationWarning {
                    code: "W1".into(),
                    message: "first".into(),
                },
                CompilationWarning {
                    code: "W2".into(),
                    message: "second".into(),
                },
            ],
            decisions: vec![
                CompilationDecision {
                    subject: "features".into(),
                    outcome: "enabled".into(),
                },
                CompilationDecision {
                    subject: "models".into(),
                    outcome: "selected".into(),
                },
            ],
        },
    ))
    .unwrap();

    assert_eq!(first.resolved_digest(), second.resolved_digest());
}

#[test]
fn resolved_digest_orders_capability_requiredness_for_equal_identity_and_resources() {
    let report = CompilationReport {
        warnings: Vec::new(),
        decisions: Vec::new(),
    };
    let mut first_parts = resolved_parts(Vec::new(), Vec::new(), report.clone());
    first_parts.capability_requirements =
        vec![capability_requirement(true), capability_requirement(false)];
    let first = ResolvedAgent::try_new(first_parts).unwrap();
    let mut second_parts = resolved_parts(Vec::new(), Vec::new(), report);
    second_parts.capability_requirements =
        vec![capability_requirement(false), capability_requirement(true)];
    let second = ResolvedAgent::try_new(second_parts).unwrap();

    assert_eq!(first.resolved_digest(), second.resolved_digest());
}

fn resolved_parts(
    resources: Vec<&str>,
    subagents: Vec<ResolvedSubagent>,
    compilation_report: CompilationReport,
) -> ResolvedAgentParts {
    ResolvedAgentParts {
        schema_version: IrSchemaVersion::V1Alpha1,
        package_identity: package_identity("parent", "1.0.0"),
        portable_digest: Sha256Digest::from_bytes([1; Sha256Digest::BYTE_LENGTH]),
        lock_digest: Sha256Digest::from_bytes([2; Sha256Digest::BYTE_LENGTH]),
        binding_digest: Sha256Digest::from_bytes([3; Sha256Digest::BYTE_LENGTH]),
        prompts: BTreeMap::new(),
        skills: BTreeMap::new(),
        models: BTreeMap::new(),
        capability_requirements: vec![ResolvedCapabilityRequirement {
            identity: CapabilityIdentity::try_new(
                CapabilityName::new("cases.read").unwrap(),
                CapabilityReleaseVersion::new("1.0.0").unwrap(),
            )
            .unwrap(),
            required: true,
            resources: resources.into_iter().map(str::to_owned).collect(),
        }],
        subagents,
        required_features: BTreeSet::new(),
        optional_features: BTreeSet::new(),
        content_capture: ResolvedContentCaptureRequirement::default(),
        compilation_report,
    }
}

fn capability_requirement(required: bool) -> ResolvedCapabilityRequirement {
    ResolvedCapabilityRequirement {
        identity: CapabilityIdentity::try_new(
            CapabilityName::new("cases.read").unwrap(),
            CapabilityReleaseVersion::new("1.0.0").unwrap(),
        )
        .unwrap(),
        required,
        resources: vec!["team:a".into()],
    }
}

fn subagent(name: &str, version: &str) -> ResolvedSubagent {
    ResolvedSubagent {
        package_identity: package_identity(name, version),
        delegation: DelegationRequirement {
            agent: PackagePath::new("agents/child.yaml").unwrap(),
            capabilities: BTreeSet::new(),
        },
        resolved_digest: Sha256Digest::from_bytes([4; Sha256Digest::BYTE_LENGTH]),
    }
}

fn package_identity(name: &str, version: &str) -> PackageIdentity {
    PackageIdentity {
        name: AgentName::new(name).unwrap(),
        version: PackageVersion::new(version).unwrap(),
    }
}
