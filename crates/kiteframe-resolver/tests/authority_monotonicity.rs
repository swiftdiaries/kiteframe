use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
};

use kiteframe_contract::{
    AgentName, ApprovalRequirement, CapabilityCatalog, CapabilityDescriptor,
    CapabilityDescriptorParts, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CapabilityRequirement, CapabilityVersion, CatalogIdentity, CompilationReport,
    ConfirmationRequirement, ConsentRequirement, DelegationRequirement, EffectClassification,
    ExecutionMode, FreshnessRequirement, IdempotencyRequirement, IrSchemaVersion, NonEmptySet,
    PackageIdentity, PackagePath, PackageVersion, PreconditionDescriptor, ResolvedAgent,
    ResolvedAgentParts, ResolvedCapabilityRequirement, ResolvedContentCaptureRequirement,
    ResolvedSubagent, ResourceSelectorSchema, Sha256Digest,
};
use kiteframe_resolver::{CandidatePolicy, select_capabilities_with_warnings, validate_catalog};
use proptest::prelude::*;
use serde_json::json;

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
        preconditions: Vec::<PreconditionDescriptor>::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn catalog(descriptor: CapabilityDescriptor) -> kiteframe_resolver::ValidatedCatalog {
    let catalog = CapabilityCatalog::try_new(
        CatalogIdentity {
            name: "support".to_owned(),
            revision: "v1".to_owned(),
        },
        vec![descriptor],
    )
    .unwrap();
    validate_catalog(&serde_json::to_vec(&catalog).unwrap()).unwrap()
}

fn requirement(required: bool) -> CapabilityRequirement {
    CapabilityRequirement {
        name: CapabilityName::new("cases.read").unwrap(),
        version: CapabilityVersion::new("^1.0").unwrap(),
        required,
        resources: BTreeSet::new(),
    }
}

fn identity(name: &str) -> PackageIdentity {
    PackageIdentity {
        name: AgentName::new(name).unwrap(),
        version: PackageVersion::new("1.0.0").unwrap(),
    }
}

fn resolved(
    resources: BTreeSet<String>,
    delegated_capabilities: BTreeSet<CapabilityName>,
) -> ResolvedAgent {
    ResolvedAgent::try_new(ResolvedAgentParts {
        schema_version: IrSchemaVersion::V1Alpha1,
        package_identity: identity("parent"),
        portable_digest: Sha256Digest::from_bytes([1; 32]),
        lock_digest: Sha256Digest::from_bytes([2; 32]),
        binding_digest: Sha256Digest::from_bytes([3; 32]),
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
            resources: resources.into_iter().collect(),
        }],
        subagents: vec![ResolvedSubagent {
            package_identity: identity("child"),
            delegation: DelegationRequirement {
                agent: PackagePath::new("agents/child/agent.yaml").unwrap(),
                capabilities: delegated_capabilities,
            },
            resolved_digest: Sha256Digest::from_bytes([4; 32]),
        }],
        required_features: BTreeSet::new(),
        optional_features: BTreeSet::new(),
        content_capture: ResolvedContentCaptureRequirement::default(),
        compilation_report: CompilationReport {
            warnings: Vec::new(),
            decisions: Vec::new(),
        },
    })
    .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn removing_a_capability_candidate_never_increases_selection(required in any::<bool>()) {
        let requirements = [requirement(required)];
        let catalog = catalog(descriptor(300));
        let broad = select_capabilities_with_warnings(
            &requirements,
            &catalog,
            CandidatePolicy::AllowAll,
        )
        .unwrap();
        let narrowed = select_capabilities_with_warnings(
            &requirements,
            &catalog,
            CandidatePolicy::exact(Vec::<String>::new()),
        );

        if required {
            prop_assert!(narrowed.is_err());
        } else {
            prop_assert!(narrowed.unwrap().selected().len() <= broad.selected().len());
        }
    }

    #[test]
    fn removing_resource_selectors_never_increases_resolved_resources(keep in 0_usize..4) {
        let broad_resources: BTreeSet<_> = ["tenant:a", "tenant:b", "tenant:c"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let narrow_resources: BTreeSet<_> = broad_resources.iter().take(keep).cloned().collect();
        let broad = resolved(broad_resources, BTreeSet::new());
        let narrow = resolved(narrow_resources.clone(), BTreeSet::new());
        let actual: BTreeSet<_> = narrow.capability_requirements()[0]
            .resources
            .iter()
            .cloned()
            .collect();
        let baseline: BTreeSet<_> = broad.capability_requirements()[0]
            .resources
            .iter()
            .cloned()
            .collect();

        prop_assert_eq!(&actual, &narrow_resources);
        prop_assert!(actual.is_subset(&baseline));
    }

    #[test]
    fn shortening_expiry_metadata_never_increases_the_freshness_window(
        short in 1_u64..300,
        extension in 0_u64..300,
    ) {
        let long = short + extension;
        let narrow = serde_json::to_value(descriptor(short)).unwrap();
        let broad = serde_json::to_value(descriptor(long)).unwrap();
        let narrow_window = narrow["freshness"]["maxAdmissionAgeSeconds"].as_u64().unwrap();
        let broad_window = broad["freshness"]["maxAdmissionAgeSeconds"].as_u64().unwrap();

        prop_assert!(narrow_window <= broad_window);
    }

    #[test]
    fn removing_delegated_capabilities_never_increases_the_child_envelope(keep in 0_usize..4) {
        let broad_capabilities: BTreeSet<_> = ["cases.read", "cases.comment", "cases.close"]
            .into_iter()
            .map(|name| CapabilityName::new(name).unwrap())
            .collect();
        let narrow_capabilities: BTreeSet<_> =
            broad_capabilities.iter().take(keep).cloned().collect();
        let broad = resolved(BTreeSet::new(), broad_capabilities);
        let narrow = resolved(BTreeSet::new(), narrow_capabilities.clone());
        let actual = &narrow.subagents()[0].delegation.capabilities;
        let baseline = &broad.subagents()[0].delegation.capabilities;

        prop_assert_eq!(actual, &narrow_capabilities);
        prop_assert!(actual.is_subset(baseline));
    }
}
