use std::collections::{BTreeMap, BTreeSet};

use _native::{
    ProviderResponseError, PyAdmissionRequest, PyCapabilityCatalog, PyCatalogRequest,
    PyInvocationRequest, load_capability_catalog_inner, load_catalog_request_inner,
    load_invocation_request_inner,
};
use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    CapabilityCatalog, CapabilityDescriptor, CapabilityDescriptorParts, CapabilityIdentity,
    CapabilityName, CapabilityReleaseVersion, CatalogIdentity, CatalogRequest,
    ConfirmationRequirement, ConsentRequirement, DelegationAncestry, EffectClassification,
    EvidenceReferences, ExecutionMode, IdempotencyRequirement, InvocationId, InvocationRequest,
    NonEmptySet, NormalizedResourceSelector, RequestedCapability, ResourceSelectorSchema,
    SessionRef, Sha256Digest, TaskRef, TraceContext,
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
fn provider_request_boundary_rejects_noncanonical_bytes() {
    let mut bytes = vec![b' '];
    bytes.extend(canonical_json(&invocation_request()).unwrap());

    assert_eq!(
        load_invocation_request_inner(&bytes).unwrap_err(),
        ProviderResponseError::NonCanonical
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
        resolved_requirements: vec![kiteframe_contract::ResolvedCapabilityRequirement {
            identity: capability_identity(),
            required: true,
            resources: vec![String::from("tenant:t1/case:case-1")],
        }],
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

fn descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability_identity(),
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
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.comment").unwrap(),
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
