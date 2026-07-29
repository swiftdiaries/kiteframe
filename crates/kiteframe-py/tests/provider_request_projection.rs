use std::collections::{BTreeMap, BTreeSet};

use _native::{
    ProviderResponseError, PyAdmissionRequest, PyCapabilityCatalog, PyCatalogRequest,
    PyInvocationRequest, PyResolvedAgent, load_capability_catalog_inner,
    load_capability_grant_set_for_request_inner, load_catalog_request_inner,
    load_invocation_outcome_for_request_inner, load_invocation_request_inner,
    load_invocation_status_for_invocation_id_inner,
};
use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    CapabilityCatalog, CapabilityDescriptor, CapabilityDescriptorParts, CapabilityGrant,
    CapabilityGrantParts, CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity,
    CapabilityName, CapabilityReleaseVersion, CatalogIdentity, CatalogRequest,
    ConfirmationRequirement, ConsentRequirement, DelegationAncestry, EffectClassification,
    EvidenceReferences, ExecutionMode, IdempotencyRequirement, InvocationId, InvocationOutcome,
    InvocationRequest, InvocationStatus, LockedCapability, NonEmptySet, NormalizedResourceSelector,
    PolicyRevision, RequestedCapability, ResolvedAgent, ResolvedCapabilityRequirement,
    ResourceSelectorSchema, SessionRef, Sha256Digest, TaskRef, Timestamp, TraceContext,
};
use kiteframe_core::canonical_json;
use pyo3::prelude::*;
use serde_json::json;

#[test]
fn request_and_catalog_projections_expose_only_stable_values() {
    let catalog_request = PyCatalogRequest::from(catalog_request());
    let admission = PyAdmissionRequest::from(admission_request());
    let invocation = PyInvocationRequest::from(invocation_request());
    let catalog = PyCapabilityCatalog::from(capability_catalog());
    let expected_catalog_digest = "09".repeat(32);

    assert_eq!(
        catalog_request.known_catalog_digest().as_deref(),
        Some(expected_catalog_digest.as_str())
    );
    assert_eq!(catalog_request.traceparent(), valid_traceparent());
    assert_eq!(admission.traceparent(), valid_traceparent());
    assert_eq!(invocation.invocation_id(), "inv-1");
    assert_eq!(invocation.admission_id(), "adm-1");
    assert_eq!(invocation.capability_name(), "cases.comment");
    assert_eq!(invocation.capability_version(), "1.0.0");
    assert_eq!(catalog.name(), "provider.test");
    assert_eq!(catalog.revision(), "revision-1");
    Python::attach(|py| {
        assert_eq!(
            admission
                .required_capabilities(py)
                .unwrap()
                .extract::<Vec<(String, String)>>()
                .unwrap(),
            vec![(String::from("cases.comment"), String::from("1.0.0"))]
        );
        assert_eq!(
            catalog
                .descriptor_digests(py)
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap()
                .len(),
            1
        );
    });
}

#[test]
fn resolved_requirement_projection_exposes_exact_locked_semantics() {
    let descriptor = descriptor();
    let requirement = ResolvedCapabilityRequirement::try_new(
        LockedCapability::try_new(
            descriptor.identity().clone(),
            descriptor.clone(),
            *descriptor.descriptor_digest(),
            Sha256Digest::from_bytes([4; Sha256Digest::BYTE_LENGTH]),
            Sha256Digest::from_bytes([5; Sha256Digest::BYTE_LENGTH]),
            Sha256Digest::from_bytes([6; Sha256Digest::BYTE_LENGTH]),
            Sha256Digest::from_bytes([7; Sha256Digest::BYTE_LENGTH]),
        )
        .unwrap(),
        true,
        vec!["tenant:t1".to_owned()],
    )
    .unwrap();
    let projected = _native::PyResolvedCapabilityRequirement::from(requirement);

    assert_eq!(
        projected.descriptor_digest(),
        descriptor.descriptor_digest().to_string()
    );
    assert_eq!(projected.input_schema_digest(), "04".repeat(32));
    assert_eq!(projected.output_schema_digest(), "05".repeat(32));
    assert_eq!(projected.stable_error_set_digest(), "06".repeat(32));
    assert_eq!(projected.safety_metadata_digest(), "07".repeat(32));
}

#[test]
fn resolved_agent_projection_exposes_verified_catalog_correlation() {
    let resolved: ResolvedAgent = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/resolved/support-agent.json"
    ))
    .unwrap();
    let projected = PyResolvedAgent::from(resolved);

    assert_eq!(projected.catalog_name(), "support");
    assert_eq!(projected.catalog_revision(), "v1");
    assert_eq!(
        projected.catalog_digest(),
        "6710f3261083b4b1a2c202e185b234bb1d58ed479962995fa3613422b503894b"
    );
}

#[test]
fn provider_request_boundary_rejects_noncanonical_bytes() {
    let mut bytes = vec![b' '];
    bytes.extend(canonical_json(&invocation_request()).unwrap());

    assert_eq!(
        load_invocation_request_inner(&bytes).unwrap_err(),
        ProviderResponseError::NonCanonical
    );
}

#[test]
fn canonical_admission_loader_rejects_normalizable_resource_order() {
    let mut wire = serde_json::to_value(admission_request()).unwrap();
    wire["requiredCapabilities"][0]["resources"] =
        json!(["tenant:t1/case:case-2", "tenant:t1/case:case-1"]);
    wire["resolvedRequirements"][0]["resources"] =
        json!(["tenant:t1/case:case-1", "tenant:t1/case:case-2"]);
    let bytes = canonical_json(&wire).unwrap();

    assert_eq!(
        _native::load_admission_request_inner(&bytes).unwrap_err(),
        ProviderResponseError::NonCanonical
    );
}

#[test]
fn canonical_catalog_loader_rejects_normalizable_descriptor_order() {
    let catalog = CapabilityCatalog::try_new(
        CatalogIdentity {
            name: String::from("provider.test"),
            revision: String::from("revision-1"),
        },
        vec![
            descriptor_with_name("cases.close"),
            descriptor_with_name("cases.comment"),
        ],
    )
    .unwrap();
    let mut wire = serde_json::to_value(catalog).unwrap();
    wire["descriptors"].as_array_mut().unwrap().reverse();
    let bytes = canonical_json(&wire).unwrap();

    assert_eq!(
        load_capability_catalog_inner(&bytes).unwrap_err(),
        ProviderResponseError::NonCanonical
    );
}

#[test]
fn locked_request_schemas_reject_noncanonical_trace_headers() {
    let mut uppercase = serde_json::to_value(catalog_request()).unwrap();
    uppercase["traceContext"]["traceparent"] = json!(valid_traceparent().to_uppercase());
    assert_eq!(
        load_catalog_request_inner(&canonical_json(&uppercase).unwrap()).unwrap_err(),
        ProviderResponseError::LockedSchema
    );

    let mut injected = serde_json::to_value(catalog_request()).unwrap();
    injected["traceContext"]["tracestate"] = json!("vendor=value\r\nforged=member");
    assert_eq!(
        load_catalog_request_inner(&canonical_json(&injected).unwrap()).unwrap_err(),
        ProviderResponseError::LockedSchema
    );
}

#[test]
fn correlated_admission_loader_rejects_a_valid_response_for_another_actor() {
    let mut parts = grant_set_parts();
    parts.actor = ActorRef::new("actor:bob").unwrap();
    let response = CapabilityGrantSet::try_new(parts).unwrap();

    assert_eq!(
        load_capability_grant_set_for_request_inner(
            &canonical_json(&response).unwrap(),
            &admission_request(),
        )
        .unwrap_err(),
        ProviderResponseError::Correlation
    );
}

#[test]
fn correlated_invocation_loaders_reject_another_invocation_id() {
    let request = invocation_request();
    let outcome = InvocationOutcome::Succeeded {
        invocation_id: InvocationId::new("inv-other").unwrap(),
        result: json!({"ok": true}),
    };
    assert_eq!(
        load_invocation_outcome_for_request_inner(&canonical_json(&outcome).unwrap(), &request,)
            .unwrap_err(),
        ProviderResponseError::Correlation
    );

    let status = InvocationStatus::Pending {
        invocation_id: InvocationId::new("inv-other").unwrap(),
    };
    assert_eq!(
        load_invocation_status_for_invocation_id_inner(
            &canonical_json(&status).unwrap(),
            request.invocation_id(),
        )
        .unwrap_err(),
        ProviderResponseError::Correlation
    );
}

#[test]
fn locked_schema_rejection_precedes_typed_contract_validation() {
    let mut wire = serde_json::to_value(catalog_request()).unwrap();
    wire.as_object_mut()
        .unwrap()
        .insert("schemaOnlyField".to_owned(), json!(true));
    let bytes = canonical_json(&wire).unwrap();

    assert_eq!(
        load_catalog_request_inner(&bytes).unwrap_err(),
        ProviderResponseError::LockedSchema
    );
}

#[test]
fn schema_valid_request_still_uses_rust_contract_validation() {
    let mut wire = serde_json::to_value(invocation_request()).unwrap();
    wire["traceContext"]["traceparent"] =
        json!("00-00000000000000000000000000000000-0000000000000000-00");
    let bytes = canonical_json(&wire).unwrap();

    assert_eq!(
        load_invocation_request_inner(&bytes).unwrap_err(),
        ProviderResponseError::Contract
    );
}

#[test]
fn schema_valid_catalog_still_uses_rust_digest_validation() {
    let mut wire = serde_json::to_value(capability_catalog()).unwrap();
    wire["catalogDigest"] = json!("00".repeat(32));
    let bytes = canonical_json(&wire).unwrap();

    assert_eq!(
        load_capability_catalog_inner(&bytes).unwrap_err(),
        ProviderResponseError::Contract
    );
}

fn catalog_request() -> CatalogRequest {
    CatalogRequest::new(
        Some(Sha256Digest::from_bytes([9; Sha256Digest::BYTE_LENGTH])),
        trace_context(),
    )
}

fn admission_request() -> AdmissionRequest {
    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        portable_digest: Sha256Digest::from_bytes([1; Sha256Digest::BYTE_LENGTH]),
        lock_digest: Sha256Digest::from_bytes([2; Sha256Digest::BYTE_LENGTH]),
        resolved_digest: Sha256Digest::from_bytes([3; Sha256Digest::BYTE_LENGTH]),
        required_capabilities: vec![
            RequestedCapability::try_new(
                capability_identity(),
                vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
            )
            .unwrap(),
        ],
        optional_capabilities: Vec::new(),
        resolved_requirements: vec![resolved_requirement()],
        delegation_ancestry: DelegationAncestry::default(),
        contextual_facts: BTreeMap::new(),
        trace_context: trace_context(),
    })
    .unwrap()
}

fn invocation_request() -> InvocationRequest {
    InvocationRequest::try_new(
        InvocationId::new("inv-1").unwrap(),
        AdmissionId::new("adm-1").unwrap(),
        capability_identity(),
        "tenant:t1/case:case-1",
        json!({"caseId": "case-1"}),
        BTreeMap::new(),
        None,
        EvidenceReferences::try_new(BTreeMap::from([(
            String::from("approval"),
            json!("evidence://approval/1"),
        )]))
        .unwrap(),
        trace_context(),
    )
    .unwrap()
}

fn capability_catalog() -> CapabilityCatalog {
    CapabilityCatalog::try_new(
        CatalogIdentity {
            name: String::from("provider.test"),
            revision: String::from("revision-1"),
        },
        vec![descriptor()],
    )
    .unwrap()
}

fn grant_set_parts() -> CapabilityGrantSetParts {
    CapabilityGrantSetParts {
        admission_id: AdmissionId::new("adm-1").unwrap(),
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        policy_revision: PolicyRevision::new("policy:7").unwrap(),
        catalog_digest: Sha256Digest::from_bytes([8; Sha256Digest::BYTE_LENGTH]),
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

fn descriptor() -> CapabilityDescriptor {
    descriptor_with_name("cases.comment")
}

fn resolved_requirement() -> ResolvedCapabilityRequirement {
    let descriptor = descriptor();
    ResolvedCapabilityRequirement::try_new(
        LockedCapability::try_new(
            descriptor.identity().clone(),
            descriptor.clone(),
            *descriptor.descriptor_digest(),
            Sha256Digest::from_bytes([4; Sha256Digest::BYTE_LENGTH]),
            Sha256Digest::from_bytes([5; Sha256Digest::BYTE_LENGTH]),
            Sha256Digest::from_bytes([6; Sha256Digest::BYTE_LENGTH]),
            Sha256Digest::from_bytes([7; Sha256Digest::BYTE_LENGTH]),
        )
        .unwrap(),
        true,
        vec![String::from("tenant:t1/case:case-1")],
    )
    .unwrap()
}

fn descriptor_with_name(name: &str) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability_identity_with_name(name),
        summary: String::from("Comment on a case"),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        stable_errors: Vec::new(),
        execution_modes: NonEmptySet::try_new(BTreeSet::from([ExecutionMode::Immediate])).unwrap(),
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

fn capability_identity() -> CapabilityIdentity {
    capability_identity_with_name("cases.comment")
}

fn capability_identity_with_name(name: &str) -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new(name).unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn trace_context() -> TraceContext {
    TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap()
}

fn valid_traceparent() -> &'static str {
    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
}
