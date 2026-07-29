use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
};

use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    AuthorityRevision, AuthorityRevisionSet, CapabilityDenial, CapabilityDescriptor,
    CapabilityDescriptorParts, CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity,
    CapabilityName, CapabilityReleaseVersion, CatalogIdentity, ConfirmationRequirement,
    ConsentRequirement, DelegationAncestry, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticSeverity, DiagnosticStage, EffectClassification, EffectiveCapabilityGrant,
    EffectiveCapabilityGrantParts, EvidenceRequirement, ExecutionMode, FreshnessRequirement,
    IdempotencyRequirement, InvocationId, InvocationRequest, LockedCapability, NonEmptySet,
    NormalizedResourceSelector, PreconditionDescriptor, PreconditionKind, RequestedCapability,
    RequiredEvidence, ResolvedCapabilityRequirement, ResourceSelectorSchema, RetryClass,
    SessionRef, Sha256Digest, TaskRef, Timestamp, TraceContext,
};
use serde_json::json;

#[test]
fn admission_request_digest_covers_catalog_correlation_and_is_verified() {
    let request = admission_request();
    let original = *request.request_digest();
    let mut wire = serde_json::to_value(&request).unwrap();
    wire["catalogDigest"] = json!(digest(99).to_string());

    assert_ne!(original, digest(99));
    assert!(serde_json::from_value::<AdmissionRequest>(wire).is_err());
}

#[test]
fn every_required_capability_must_have_an_effective_grant() {
    let request = admission_request();
    let mut parts = grant_set_parts(&request);
    parts
        .grants
        .retain(|grant| grant.capability() != &required_identity());
    let response = CapabilityGrantSet::try_new(parts).unwrap();

    assert_eq!(
        response
            .validate_against(&request)
            .unwrap_err()
            .code
            .as_str(),
        "KF-CAP-002"
    );
}

#[test]
fn every_optional_capability_has_exactly_one_grant_or_denial() {
    let request = admission_request();

    let mut missing = grant_set_parts(&request);
    missing.optional_denials.clear();
    assert_invalid(CapabilityGrantSet::try_new(missing).unwrap(), &request);

    let mut both = grant_set_parts(&request);
    both.grants.push(effective_grant(
        optional_identity(),
        EffectClassification::ReadOnly,
        Timestamp::new(180),
        required_evidence(),
        freshness(20, true, 10),
        required_preconditions(),
    ));
    assert_invalid(CapabilityGrantSet::try_new(both).unwrap(), &request);
}

#[test]
fn effective_grant_cannot_widen_any_locked_dimension() {
    let request = admission_request();
    let base = grant_set_parts(&request);

    let cases = [
        effective_grant_with(
            vec!["tenant:t1/case:case-1", "tenant:t1/case:case-2"],
            modes(&[ExecutionMode::Immediate]),
            EffectClassification::ReversibleWrite,
            Timestamp::new(180),
            required_evidence(),
            freshness(20, true, 10),
            required_preconditions(),
        ),
        effective_grant_with(
            vec!["tenant:t1/case:case-1"],
            modes(&[ExecutionMode::Immediate, ExecutionMode::Deferred]),
            EffectClassification::ReversibleWrite,
            Timestamp::new(180),
            required_evidence(),
            freshness(20, true, 10),
            required_preconditions(),
        ),
        effective_grant_with(
            vec!["tenant:t1/case:case-1"],
            modes(&[ExecutionMode::Immediate]),
            EffectClassification::ExternalSideEffect,
            Timestamp::new(180),
            required_evidence(),
            freshness(20, true, 10),
            required_preconditions(),
        ),
        effective_grant_with(
            vec!["tenant:t1/case:case-1"],
            modes(&[ExecutionMode::Immediate]),
            EffectClassification::ReversibleWrite,
            Timestamp::new(201),
            required_evidence(),
            freshness(20, true, 10),
            required_preconditions(),
        ),
        effective_grant_with(
            vec!["tenant:t1/case:case-1"],
            modes(&[ExecutionMode::Immediate]),
            EffectClassification::ReversibleWrite,
            Timestamp::new(180),
            RequiredEvidence::new(
                ConfirmationRequirement::None,
                ApprovalRequirement::None,
                ConsentRequirement::None,
            ),
            freshness(20, true, 10),
            required_preconditions(),
        ),
        effective_grant_with(
            vec!["tenant:t1/case:case-1"],
            modes(&[ExecutionMode::Immediate]),
            EffectClassification::ReversibleWrite,
            Timestamp::new(180),
            required_evidence(),
            freshness(31, false, 16),
            required_preconditions(),
        ),
        effective_grant_with(
            vec!["tenant:t1/case:case-1"],
            modes(&[ExecutionMode::Immediate]),
            EffectClassification::ReversibleWrite,
            Timestamp::new(180),
            required_evidence(),
            freshness(20, true, 10),
            Vec::new(),
        ),
    ];

    for grant in cases {
        let mut parts = base.clone();
        parts.grants[0] = grant;
        let response = CapabilityGrantSet::try_new(parts).unwrap();
        assert_invalid(response, &request);
    }
}

#[test]
fn authority_revisions_are_canonical_and_reject_duplicate_sources() {
    let set = AuthorityRevisionSet::try_new(vec![
        AuthorityRevision::try_new("policy", "7").unwrap(),
        AuthorityRevision::try_new("directory", "42").unwrap(),
    ])
    .unwrap();
    assert_eq!(set.entries()[0].source(), "directory");
    assert_eq!(set.entries()[1].source(), "policy");

    let reversed = AuthorityRevisionSet::try_new(vec![
        AuthorityRevision::try_new("directory", "42").unwrap(),
        AuthorityRevision::try_new("policy", "7").unwrap(),
    ])
    .unwrap();
    assert_eq!(
        set.authority_revision_digest(),
        reversed.authority_revision_digest()
    );
    assert!(
        AuthorityRevisionSet::try_new(vec![
            AuthorityRevision::try_new("policy", "7").unwrap(),
            AuthorityRevision::try_new("policy", "8").unwrap(),
        ])
        .is_err()
    );
}

#[test]
fn invocation_requires_exact_admitted_grant_digest_and_never_carries_authority_revisions() {
    let request = InvocationRequest::try_new(
        InvocationId::new("inv-1").unwrap(),
        AdmissionId::new("adm-1").unwrap(),
        digest(77),
        required_identity(),
        "tenant:t1/case:case-1",
        json!({}),
        BTreeMap::new(),
        None,
        Default::default(),
        trace_context(),
    )
    .unwrap();
    assert_eq!(request.grant_digest(), &digest(77));

    let wire = serde_json::to_value(request).unwrap();
    assert!(wire.get("admissionId").is_some());
    assert!(wire.get("grantDigest").is_some());
    assert!(wire.get("authorityRevisions").is_none());
}

#[test]
fn invocation_schema_requires_only_admission_and_grant_authority_correlation() {
    let schema: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../schemas/v1alpha1/invocation-request.schema.json"
    ))
    .unwrap();
    let required = schema["required"].as_array().unwrap();
    assert!(required.contains(&json!("admissionId")));
    assert!(required.contains(&json!("grantDigest")));
    assert!(schema["properties"].get("authorityRevisions").is_none());
}

fn assert_invalid(response: CapabilityGrantSet, request: &AdmissionRequest) {
    assert_eq!(
        response
            .validate_against(request)
            .unwrap_err()
            .code
            .as_str(),
        "KF-CAP-002"
    );
}

fn admission_request() -> AdmissionRequest {
    let required = required_requirement();
    let optional = optional_requirement();
    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        catalog_identity: catalog_identity(),
        catalog_digest: digest(4),
        required_capabilities: vec![
            RequestedCapability::try_new(
                required_identity(),
                vec![selector("tenant:t1/case:case-1")],
            )
            .unwrap(),
        ],
        optional_capabilities: vec![
            RequestedCapability::try_new(
                optional_identity(),
                vec![selector("tenant:t1/note:note-1")],
            )
            .unwrap(),
        ],
        resolved_requirements: vec![required, optional],
        delegation_ancestry: DelegationAncestry::default(),
        contextual_facts: BTreeMap::new(),
        trace_context: trace_context(),
    })
    .unwrap()
}

fn grant_set_parts(request: &AdmissionRequest) -> CapabilityGrantSetParts {
    CapabilityGrantSetParts {
        admission_id: AdmissionId::new("adm-1").unwrap(),
        admission_request_digest: *request.request_digest(),
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        policy_revision: kiteframe_contract::PolicyRevision::new("policy:7").unwrap(),
        catalog_identity: catalog_identity(),
        catalog_digest: digest(4),
        authority_revisions: AuthorityRevisionSet::try_new(vec![
            AuthorityRevision::try_new("policy", "7").unwrap(),
        ])
        .unwrap(),
        issued_at: Timestamp::new(100),
        expires_at: Timestamp::new(200),
        grants: vec![effective_grant(
            required_identity(),
            EffectClassification::ReversibleWrite,
            Timestamp::new(180),
            required_evidence(),
            freshness(20, true, 10),
            required_preconditions(),
        )],
        optional_denials: vec![
            CapabilityDenial::try_new(optional_identity(), admission_denial()).unwrap(),
        ],
    }
}

fn effective_grant(
    capability: CapabilityIdentity,
    effect: EffectClassification,
    expires_at: Timestamp,
    evidence: RequiredEvidence,
    freshness: FreshnessRequirement,
    preconditions: Vec<PreconditionDescriptor>,
) -> EffectiveCapabilityGrant {
    let resources = if capability == required_identity() {
        vec!["tenant:t1/case:case-1"]
    } else {
        vec!["tenant:t1/note:note-1"]
    };
    EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability,
        resources: resources.into_iter().map(selector).collect(),
        execution_modes: modes(&[ExecutionMode::Immediate]),
        maximum_effect: effect,
        expires_at,
        required_evidence: evidence,
        freshness,
        preconditions,
    })
    .unwrap()
}

fn effective_grant_with(
    resources: Vec<&str>,
    execution_modes: NonEmptySet<ExecutionMode>,
    maximum_effect: EffectClassification,
    expires_at: Timestamp,
    required_evidence: RequiredEvidence,
    freshness: FreshnessRequirement,
    preconditions: Vec<PreconditionDescriptor>,
) -> EffectiveCapabilityGrant {
    EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: required_identity(),
        resources: resources.into_iter().map(selector).collect(),
        execution_modes,
        maximum_effect,
        expires_at,
        required_evidence,
        freshness,
        preconditions,
    })
    .unwrap()
}

fn required_requirement() -> ResolvedCapabilityRequirement {
    resolved_requirement(required_descriptor(), true, vec!["tenant:t1/case:case-1"])
}

fn optional_requirement() -> ResolvedCapabilityRequirement {
    resolved_requirement(optional_descriptor(), false, vec!["tenant:t1/note:note-1"])
}

fn resolved_requirement(
    descriptor: CapabilityDescriptor,
    required: bool,
    resources: Vec<&str>,
) -> ResolvedCapabilityRequirement {
    let identity = descriptor.identity().clone();
    let descriptor_digest = *descriptor.descriptor_digest();
    ResolvedCapabilityRequirement::try_new(
        LockedCapability::try_new(
            identity,
            descriptor,
            descriptor_digest,
            digest(11),
            digest(12),
            digest(13),
            digest(14),
        )
        .unwrap(),
        required,
        resources.into_iter().map(ToOwned::to_owned).collect(),
    )
    .unwrap()
}

fn required_descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: required_identity(),
        summary: "Update a case".to_owned(),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        stable_errors: Vec::new(),
        execution_modes: modes(&[ExecutionMode::Immediate]),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect: EffectClassification::ReversibleWrite,
        idempotency: IdempotencyRequirement::Required {
            scope: kiteframe_contract::IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: NonZeroU64::new(60).unwrap(),
        },
        freshness: freshness(30, true, 15),
        preconditions: required_preconditions(),
        confirmation: required_confirmation(),
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn optional_descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: optional_identity(),
        summary: "Read a note".to_owned(),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        stable_errors: Vec::new(),
        execution_modes: modes(&[ExecutionMode::Immediate]),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect: EffectClassification::ReadOnly,
        idempotency: IdempotencyRequirement::None,
        freshness: Default::default(),
        preconditions: Vec::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn required_evidence() -> RequiredEvidence {
    RequiredEvidence::new(
        required_confirmation(),
        ApprovalRequirement::None,
        ConsentRequirement::None,
    )
}

fn required_confirmation() -> ConfirmationRequirement {
    ConfirmationRequirement::Required {
        evidence: EvidenceRequirement {
            kind: "confirmation".to_owned(),
            issuer: Some("user".to_owned()),
        },
    }
}

fn required_preconditions() -> Vec<PreconditionDescriptor> {
    vec![PreconditionDescriptor {
        name: "caseVersion".to_owned(),
        kind: PreconditionKind::EntityVersion,
        required: true,
    }]
}

fn freshness(admission: u64, policy_revision_required: bool, input: u64) -> FreshnessRequirement {
    FreshnessRequirement {
        max_admission_age_seconds: NonZeroU64::new(admission),
        policy_revision_required,
        max_input_age_seconds: NonZeroU64::new(input),
    }
}

fn admission_denial() -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::AdmissionDenied,
        category: DiagnosticCategory::Authorization,
        severity: DiagnosticSeverity::Warning,
        stage: DiagnosticStage::Admit,
        package_path: None,
        source_range: None,
        message: "optional capability denied".into(),
        help: None,
        retry: RetryClass::Never,
        details: BTreeMap::new(),
    }
}

fn modes(values: &[ExecutionMode]) -> NonEmptySet<ExecutionMode> {
    NonEmptySet::try_new(values.iter().copied().collect::<BTreeSet<_>>()).unwrap()
}

fn selector(value: &str) -> NormalizedResourceSelector {
    NormalizedResourceSelector::new(value).unwrap()
}

fn catalog_identity() -> CatalogIdentity {
    CatalogIdentity {
        name: "test-catalog".to_owned(),
        revision: "r1".to_owned(),
    }
}

fn required_identity() -> CapabilityIdentity {
    identity("cases.update")
}

fn optional_identity() -> CapabilityIdentity {
    identity("notes.read")
}

fn identity(name: &str) -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new(name).unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn trace_context() -> TraceContext {
    TraceContext::try_new(
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        None,
        BTreeMap::new(),
    )
    .unwrap()
}

fn digest(value: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([value; Sha256Digest::BYTE_LENGTH])
}
