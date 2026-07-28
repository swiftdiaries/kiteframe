use _native::{
    ProviderResponseError, PyCapabilityGrantSet, PyInvocationOutcome, PyInvocationStatus,
    load_capability_grant_set_inner, load_invocation_outcome_inner, load_invocation_status_inner,
};
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, CapabilityGrant, CapabilityGrantParts, CapabilityGrantSet,
    CapabilityGrantSetParts, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    InvocationOutcome, InvocationStatus, NormalizedResourceSelector, PolicyRevision, SessionRef,
    Sha256Digest, TaskRef, Timestamp,
};

#[test]
fn grant_set_projection_exposes_only_stable_scalar_and_tuple_values() {
    let projection = PyCapabilityGrantSet::from(grant_set());

    assert_eq!(projection.admission_id(), "adm-1");
    assert_eq!(projection.actor(), "actor:alice");
    assert_eq!(projection.issued_at(), 100);
    assert_eq!(projection.expires_at(), 200);
    assert!(
        projection
            .canonical_json()
            .unwrap()
            .windows(b"cases.comment".len())
            .any(|window| window == b"cases.comment")
    );
}

#[test]
fn invocation_loaders_validate_and_preserve_stable_variants() {
    let outcome =
        load_invocation_outcome_inner(br#"{"invocation_id":"inv-1","status":"deferred"}"#).unwrap();
    let status =
        load_invocation_status_inner(br#"{"invocation_id":"inv-1","status":"pending"}"#).unwrap();

    assert!(matches!(outcome, InvocationOutcome::Deferred { .. }));
    assert!(matches!(status, InvocationStatus::Pending { .. }));
    assert_eq!(PyInvocationOutcome::from(outcome).status(), "deferred");
    assert_eq!(PyInvocationStatus::from(status).status(), "pending");
}

#[test]
fn provider_loaders_reject_contract_invalid_response_after_schema_validation() {
    assert_eq!(
        load_invocation_outcome_inner(br#"{"invocation_id":" ","status":"deferred"}"#).unwrap_err(),
        ProviderResponseError::Contract
    );
}

#[test]
fn grant_set_response_boundary_rejects_locked_schema_violation() {
    let mut response = serde_json::to_value(grant_set()).unwrap();
    response
        .as_object_mut()
        .unwrap()
        .insert("schemaOnlyField".to_owned(), serde_json::json!(true));

    assert_eq!(
        load_capability_grant_set_inner(&serde_json::to_vec(&response).unwrap()).unwrap_err(),
        ProviderResponseError::LockedSchema
    );
}

#[test]
fn invocation_outcome_response_boundary_rejects_locked_schema_violation() {
    let response = br#"{"invocation_id":"inv-1","status":"deferred","schema_only_field":true}"#;

    assert_eq!(
        load_invocation_outcome_inner(response).unwrap_err(),
        ProviderResponseError::LockedSchema
    );
}

#[test]
fn invocation_status_response_boundary_rejects_locked_schema_violation() {
    let response = br#"{"invocation_id":"inv-1","status":"pending","schema_only_field":true}"#;

    assert_eq!(
        load_invocation_status_inner(response).unwrap_err(),
        ProviderResponseError::LockedSchema
    );
}

fn grant_set() -> CapabilityGrantSet {
    CapabilityGrantSet::try_new(CapabilityGrantSetParts {
        admission_id: AdmissionId::new("adm-1").unwrap(),
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        policy_revision: PolicyRevision::new("policy:7").unwrap(),
        catalog_digest: Sha256Digest::from_bytes([1; 32]),
        issued_at: Timestamp::new(100),
        expires_at: Timestamp::new(200),
        grants: vec![
            CapabilityGrant::try_new(CapabilityGrantParts {
                capability: CapabilityIdentity::try_new(
                    CapabilityName::new("cases.comment").unwrap(),
                    CapabilityReleaseVersion::new("1.0.0").unwrap(),
                )
                .unwrap(),
                resources: vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
            })
            .unwrap(),
        ],
    })
    .unwrap()
}
