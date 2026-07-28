use std::collections::{BTreeMap, BTreeSet};

use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    CapabilityDescriptor, CapabilityDescriptorParts, CapabilityGrant, CapabilityGrantParts,
    CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity, CapabilityName,
    CapabilityReleaseVersion, ConfirmationRequirement, ConsentRequirement, DelegationAncestry,
    Diagnostic, EffectClassification, EvidenceReferences, EvidenceRequirement, ExecutionMode,
    IdempotencyRequirement, IdempotencyScope, InvocationId, InvocationOutcome, InvocationRequest,
    InvocationStatus, NonEmptySet, NormalizedResourceSelector, PolicyRevision,
    PreconditionDescriptor, RequestedCapability, ResourceSelectorSchema, RetryClass, SessionRef,
    Sha256Digest, TaskRef, Timestamp, TraceContext,
};
use serde_json::json;

#[test]
fn grant_set_is_time_bounded_and_not_a_bearer_credential() {
    let schema = serde_json::to_string(&schemars::schema_for!(
        kiteframe_contract::CapabilityGrantSet
    ))
    .unwrap();
    assert!(schema.contains("issuedAt"));
    assert!(schema.contains("expiresAt"));
    assert!(schema.contains("policyRevision"));
    assert!(!schema.contains("token"));
    assert!(!schema.contains("credential"));
}

#[test]
fn effectful_invocation_requires_an_idempotency_key() {
    let descriptor = effectful_descriptor();
    let request = invocation_request(None);
    let errors = request.validate_against(&descriptor).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn outcome_unknown_requires_status_first_retry() {
    let outcome = InvocationOutcome::OutcomeUnknown {
        invocation_id: InvocationId::new("inv-1").unwrap(),
        diagnostic: Diagnostic::outcome_unknown("status is required"),
    };
    assert_eq!(outcome.diagnostic().unwrap().retry, RetryClass::StatusFirst);
}

#[test]
fn invocation_status_schema_has_only_the_stable_status_vocabulary() {
    let schema = serde_json::to_string(&schemars::schema_for!(InvocationStatus)).unwrap();
    for status in [
        "pending",
        "suspended",
        "succeeded",
        "failed",
        "denied",
        "outcome_unknown",
    ] {
        assert!(schema.contains(status));
    }
    assert!(!schema.contains("deferred"));
}

#[test]
fn trace_context_rejects_non_allowlisted_baggage() {
    let baggage = BTreeMap::from([(String::from("credential"), String::from("secret"))]);
    assert!(TraceContext::try_new(valid_traceparent(), None, baggage).is_err());
}

#[test]
fn trace_context_deserialization_rejects_non_allowlisted_baggage() {
    let trace = serde_json::from_value::<TraceContext>(json!({
        "traceparent": valid_traceparent(),
        "baggage": {"authorization": "tuple"}
    }));
    assert!(trace.is_err());
}

#[test]
fn trace_context_rejects_an_opaque_bearer_secret_in_allowlisted_baggage() {
    let baggage = BTreeMap::from([(
        String::from("kiteframe.session_id"),
        String::from("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhbGljZSJ9.signature"),
    )]);
    assert!(TraceContext::try_new(valid_traceparent(), None, baggage).is_err());
}

#[test]
fn evidence_references_reject_raw_payloads() {
    assert!(
        EvidenceReferences::try_new(BTreeMap::from([(
            String::from("approval"),
            json!({"signedBy": "alice"}),
        )]))
        .is_err()
    );
}

#[test]
fn idempotency_key_is_forbidden_when_descriptor_does_not_support_it() {
    let descriptor = read_only_descriptor();
    let request = invocation_request(Some("idem-1"));
    let errors = request.validate_against(&descriptor).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn grant_set_rejects_expiry_at_or_before_issue_time() {
    let mut parts = grant_set_parts();
    parts.expires_at = parts.issued_at;
    let errors = CapabilityGrantSet::try_new(parts).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn grant_set_rejects_duplicate_capability_versions() {
    let mut parts = grant_set_parts();
    parts.grants.push(parts.grants[0].clone());
    let errors = CapabilityGrantSet::try_new(parts).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn admission_rejects_resource_selector_broader_than_resolved_requirement() {
    let request = RequestedCapability::try_new(
        capability_identity(),
        vec![NormalizedResourceSelector::new("tenant:t1/case:*").unwrap()],
    )
    .unwrap();
    let errors = AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        required_capabilities: vec![request],
        optional_capabilities: Vec::new(),
        resolved_requirements: vec![kiteframe_contract::ResolvedCapabilityRequirement {
            identity: capability_identity(),
            required: true,
            resources: vec![String::from("tenant:t1/case:case-1")],
        }],
        delegation_ancestry: DelegationAncestry::default(),
        contextual_facts: BTreeMap::new(),
        trace_context: TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap(),
    })
    .unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn admission_deserialization_rejects_duplicate_capability_versions() {
    let mut wire = serde_json::to_value(valid_admission_request()).unwrap();
    let required = wire["requiredCapabilities"].as_array_mut().unwrap();
    required.push(required[0].clone());
    assert!(serde_json::from_value::<AdmissionRequest>(wire).is_err());
}

#[test]
fn admission_deserialization_rejects_a_selector_broader_than_its_resolved_requirement() {
    let mut wire = serde_json::to_value(valid_admission_request()).unwrap();
    wire["requiredCapabilities"][0]["resources"] = json!(["tenant:t1/case:*"]);
    let error = serde_json::from_value::<AdmissionRequest>(wire).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("broader than the resolved requirement")
    );
}

#[test]
fn capability_grant_deserialization_rejects_empty_resources() {
    let mut wire = serde_json::to_value(grant_set_parts().grants.remove(0)).unwrap();
    wire["resources"] = json!([]);
    assert!(serde_json::from_value::<CapabilityGrant>(wire).is_err());
}

#[test]
fn outcome_unknown_deserialization_rejects_non_status_first_retry() {
    let wire = json!({
        "status": "outcome_unknown",
        "invocation_id": "inv-1",
        "diagnostic": diagnostic_with_retry("never")
    });
    assert!(serde_json::from_value::<InvocationOutcome>(wire).is_err());
}

#[test]
fn status_unknown_deserialization_rejects_non_status_first_retry() {
    let wire = json!({
        "status": "outcome_unknown",
        "invocation_id": "inv-1",
        "diagnostic": diagnostic_with_retry("never")
    });
    assert!(serde_json::from_value::<InvocationStatus>(wire).is_err());
}

fn invocation_request(idempotency_key: Option<&str>) -> InvocationRequest {
    InvocationRequest::try_new(
        InvocationId::new("inv-1").unwrap(),
        AdmissionId::new("adm-1").unwrap(),
        capability_identity(),
        "tenant:t1/case:case-1",
        json!({"caseId": "case-1"}),
        BTreeMap::new(),
        idempotency_key.map(ToOwned::to_owned),
        EvidenceReferences::try_new(BTreeMap::from([(
            String::from("approval"),
            json!("evidence://approval/1"),
        )]))
        .unwrap(),
        TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap(),
    )
    .unwrap()
}

fn effectful_descriptor() -> CapabilityDescriptor {
    descriptor(
        EffectClassification::ExternalSideEffect,
        IdempotencyRequirement::Required {
            scope: IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: std::num::NonZeroU64::new(60).unwrap(),
        },
    )
}

fn read_only_descriptor() -> CapabilityDescriptor {
    descriptor(EffectClassification::ReadOnly, IdempotencyRequirement::None)
}

fn descriptor(
    effect: EffectClassification,
    idempotency: IdempotencyRequirement,
) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability_identity(),
        summary: String::from("Operate on a case"),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        stable_errors: Vec::new(),
        execution_modes: NonEmptySet::try_new(BTreeSet::from([ExecutionMode::Immediate])).unwrap(),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect,
        idempotency,
        freshness: Default::default(),
        preconditions: Vec::<PreconditionDescriptor>::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::Required {
            evidence: EvidenceRequirement {
                kind: String::from("approval"),
                issuer: None,
            },
        },
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn capability_identity() -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.comment").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn valid_traceparent() -> String {
    String::from("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
}

fn valid_admission_request() -> AdmissionRequest {
    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        required_capabilities: vec![
            RequestedCapability::try_new(
                capability_identity(),
                vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
            )
            .unwrap(),
        ],
        optional_capabilities: Vec::new(),
        resolved_requirements: vec![kiteframe_contract::ResolvedCapabilityRequirement {
            identity: capability_identity(),
            required: true,
            resources: vec![String::from("tenant:t1/case:case-1")],
        }],
        delegation_ancestry: DelegationAncestry::default(),
        contextual_facts: BTreeMap::new(),
        trace_context: TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap(),
    })
    .unwrap()
}

fn diagnostic_with_retry(retry: &str) -> serde_json::Value {
    json!({
        "code": "KF-CAP-003",
        "category": "capability",
        "severity": "error",
        "stage": "invoke",
        "package_path": null,
        "source_range": null,
        "message": "status is required",
        "help": null,
        "retry": retry,
        "details": {}
    })
}

fn grant_set_parts() -> CapabilityGrantSetParts {
    CapabilityGrantSetParts {
        admission_id: AdmissionId::new("adm-1").unwrap(),
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        policy_revision: PolicyRevision::new("policy:7").unwrap(),
        catalog_digest: digest(1),
        issued_at: Timestamp::new(100),
        expires_at: Timestamp::new(200),
        grants: vec![
            CapabilityGrant::try_new(CapabilityGrantParts {
                capability: capability_identity(),
                resources: vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
            })
            .unwrap(),
        ],
    }
}

fn digest(value: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([value; Sha256Digest::BYTE_LENGTH])
}
