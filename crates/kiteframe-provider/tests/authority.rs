use std::{collections::BTreeSet, num::NonZeroU64};

use kiteframe_contract::{
    ApprovalRequirement, CapabilityDescriptor, CapabilityDescriptorParts, CapabilityIdentity,
    CapabilityName, CapabilityReleaseVersion, ConfirmationRequirement, ConsentRequirement,
    EffectClassification, EffectiveCapabilityGrant, EffectiveCapabilityGrantParts,
    EvidenceRequirement, ExecutionMode, FreshnessRequirement, IdempotencyRequirement,
    IdempotencyScope, LockedCapability, NonEmptySet, NormalizedResourceSelector,
    PreconditionDescriptor, PreconditionKind, RequiredEvidence, ResolvedCapabilityRequirement,
    ResourceSelectorSchema, Sha256Digest, Timestamp,
};
use kiteframe_provider::{AuthorityTerm, EffectiveGrantSubset, intersect_authority};
use proptest::prelude::*;
use serde_json::json;

const HOUR_1: Timestamp = Timestamp::new(3_600);
const HOUR_2: Timestamp = Timestamp::new(7_200);

#[test]
fn explicit_deny_wins_over_allows() {
    let terms = vec![
        AuthorityTerm::allow(grant(
            "tenant:t1/case:*",
            HOUR_2,
            EffectClassification::ReadOnly,
            evidence_none(),
        )),
        AuthorityTerm::deny("cases.read"),
        AuthorityTerm::allow(grant(
            "tenant:t1/case:case-1",
            HOUR_1,
            EffectClassification::ReadOnly,
            evidence_none(),
        )),
    ];

    assert!(
        intersect_authority(&resolved_requirement(), &terms)
            .unwrap()
            .is_none()
    );
}

#[test]
fn narrower_resource_expiry_and_evidence_win() {
    let effective = intersect_authority(
        &resolved_requirement(),
        &[
            AuthorityTerm::allow(grant(
                "tenant:t1/case:*",
                HOUR_2,
                EffectClassification::ReversibleWrite,
                confirmation_evidence(),
            )),
            AuthorityTerm::allow(grant(
                "tenant:t1/case:case-7",
                HOUR_1,
                EffectClassification::ReadOnly,
                approval_evidence(),
            )),
        ],
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        effective
            .resources()
            .iter()
            .map(NormalizedResourceSelector::as_str)
            .collect::<Vec<_>>(),
        ["tenant:t1/case:case-7"]
    );
    assert_eq!(effective.expires_at(), HOUR_1);
    assert_eq!(
        effective.required_evidence().approval(),
        approval_evidence().approval()
    );
    assert_eq!(
        effective.required_evidence().confirmation(),
        confirmation_evidence().confirmation()
    );
    assert_eq!(effective.maximum_effect(), EffectClassification::ReadOnly);
}

#[test]
fn intersection_narrows_modes_freshness_and_preconditions() {
    let effective = intersect_authority(
        &resolved_requirement(),
        &[
            AuthorityTerm::allow(grant_with_axes(
                &["tenant:t1/case:*"],
                &[ExecutionMode::Immediate, ExecutionMode::Deferred],
                HOUR_2,
                FreshnessRequirement {
                    max_admission_age_seconds: Some(NonZeroU64::new(600).unwrap()),
                    policy_revision_required: false,
                    max_input_age_seconds: None,
                },
                vec![precondition("etag", PreconditionKind::Etag, true)],
            )),
            AuthorityTerm::allow(grant_with_axes(
                &["tenant:t1/case:case-7"],
                &[ExecutionMode::Immediate],
                HOUR_1,
                FreshnessRequirement {
                    max_admission_age_seconds: Some(NonZeroU64::new(300).unwrap()),
                    policy_revision_required: true,
                    max_input_age_seconds: Some(NonZeroU64::new(30).unwrap()),
                },
                vec![precondition(
                    "entityVersion",
                    PreconditionKind::EntityVersion,
                    true,
                )],
            )),
        ],
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        effective.execution_modes().as_set(),
        &BTreeSet::from([ExecutionMode::Immediate])
    );
    assert_eq!(
        effective.freshness(),
        &FreshnessRequirement {
            max_admission_age_seconds: Some(NonZeroU64::new(300).unwrap()),
            policy_revision_required: true,
            max_input_age_seconds: Some(NonZeroU64::new(30).unwrap()),
        }
    );
    assert_eq!(
        effective.preconditions(),
        [
            precondition("entityVersion", PreconditionKind::EntityVersion, true),
            precondition("etag", PreconditionKind::Etag, true),
        ]
    );
}

#[test]
fn unresolved_context_placeholders_are_rejected() {
    let errors = intersect_authority(
        &resolved_requirement(),
        &[AuthorityTerm::allow(grant(
            "tenant:${context.tenant}/case:*",
            HOUR_1,
            EffectClassification::ReadOnly,
            evidence_none(),
        ))],
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-AUTH-001");
}

#[test]
fn disjoint_resource_authority_denies_instead_of_widening() {
    let result = intersect_authority(
        &resolved_requirement(),
        &[
            AuthorityTerm::allow(grant(
                "tenant:t1/case:case-7",
                HOUR_2,
                EffectClassification::ReadOnly,
                evidence_none(),
            )),
            AuthorityTerm::allow(grant(
                "tenant:t1/case:case-8",
                HOUR_1,
                EffectClassification::ReadOnly,
                evidence_none(),
            )),
        ],
    )
    .unwrap();

    assert!(result.is_none());
}

proptest! {
    #[test]
    fn adding_a_restriction_never_increases_envelope(
        base in authority_term_strategy(),
        restriction in narrower_term_strategy(),
    ) {
        let requirement = resolved_requirement();
        let before = intersect_authority(&requirement, std::slice::from_ref(&base))
            .unwrap()
            .unwrap();
        let after = intersect_authority(&requirement, &[base, restriction])
            .unwrap()
            .unwrap();
        prop_assert!(after.is_subset_of(&before));
    }
}

fn authority_term_strategy() -> impl Strategy<Value = AuthorityTerm> {
    (
        prop_oneof![Just("tenant:t1/case:*"), Just("tenant:t1/case:case-7")],
        3_600_u64..=7_200,
        prop_oneof![
            Just(EffectClassification::ReadOnly),
            Just(EffectClassification::ReversibleWrite),
        ],
    )
        .prop_map(|(resource, expiry, effect)| {
            AuthorityTerm::allow(grant(
                resource,
                Timestamp::new(expiry),
                effect,
                evidence_none(),
            ))
        })
}

fn narrower_term_strategy() -> impl Strategy<Value = AuthorityTerm> {
    Just(AuthorityTerm::allow(grant(
        "tenant:t1/case:case-7",
        HOUR_1,
        EffectClassification::ReadOnly,
        approval_evidence(),
    )))
}

fn resolved_requirement() -> ResolvedCapabilityRequirement {
    ResolvedCapabilityRequirement::try_new(
        locked_capability(),
        true,
        vec!["tenant:t1/case:*".to_owned()],
    )
    .unwrap()
}

fn locked_capability() -> LockedCapability {
    let descriptor = descriptor();
    LockedCapability::try_new(
        descriptor.identity().clone(),
        descriptor.clone(),
        *descriptor.descriptor_digest(),
        digest(1),
        digest(2),
        digest(3),
        digest(4),
    )
    .unwrap()
}

fn descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability_identity(),
        summary: "Read cases".to_owned(),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        stable_errors: vec![],
        execution_modes: modes(&[ExecutionMode::Immediate, ExecutionMode::Deferred]),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect: EffectClassification::ReversibleWrite,
        idempotency: IdempotencyRequirement::Required {
            scope: IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: NonZeroU64::new(3_600).unwrap(),
        },
        freshness: FreshnessRequirement::default(),
        preconditions: vec![],
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn capability_identity() -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.read").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn grant(
    resource: &str,
    expires_at: Timestamp,
    maximum_effect: EffectClassification,
    required_evidence: RequiredEvidence,
) -> EffectiveCapabilityGrant {
    EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: capability_identity(),
        resources: vec![selector(resource)],
        execution_modes: modes(&[ExecutionMode::Immediate, ExecutionMode::Deferred]),
        maximum_effect,
        expires_at,
        required_evidence,
        freshness: FreshnessRequirement::default(),
        preconditions: vec![],
    })
    .unwrap()
}

fn grant_with_axes(
    resources: &[&str],
    execution_modes: &[ExecutionMode],
    expires_at: Timestamp,
    freshness: FreshnessRequirement,
    preconditions: Vec<PreconditionDescriptor>,
) -> EffectiveCapabilityGrant {
    EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: capability_identity(),
        resources: resources.iter().map(|value| selector(value)).collect(),
        execution_modes: modes(execution_modes),
        maximum_effect: EffectClassification::ReadOnly,
        expires_at,
        required_evidence: evidence_none(),
        freshness,
        preconditions,
    })
    .unwrap()
}

fn modes(values: &[ExecutionMode]) -> NonEmptySet<ExecutionMode> {
    NonEmptySet::try_new(values.iter().copied().collect()).unwrap()
}

fn selector(value: &str) -> NormalizedResourceSelector {
    NormalizedResourceSelector::new(value).unwrap()
}

fn precondition(name: &str, kind: PreconditionKind, required: bool) -> PreconditionDescriptor {
    PreconditionDescriptor {
        name: name.to_owned(),
        kind,
        required,
    }
}

fn evidence_none() -> RequiredEvidence {
    RequiredEvidence::new(
        ConfirmationRequirement::None,
        ApprovalRequirement::None,
        ConsentRequirement::None,
    )
}

fn confirmation_evidence() -> RequiredEvidence {
    RequiredEvidence::new(
        ConfirmationRequirement::Required {
            evidence: EvidenceRequirement {
                kind: "case_confirmation".to_owned(),
                issuer: None,
            },
        },
        ApprovalRequirement::None,
        ConsentRequirement::None,
    )
}

fn approval_evidence() -> RequiredEvidence {
    RequiredEvidence::new(
        ConfirmationRequirement::None,
        ApprovalRequirement::Required {
            evidence: EvidenceRequirement {
                kind: "manager_approval".to_owned(),
                issuer: Some("approvals".to_owned()),
            },
        },
        ConsentRequirement::None,
    )
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}
