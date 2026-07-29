use _native::{
    ProviderResponseError, PyCapabilityGrantSet, PyInvocationOutcome, PyInvocationStatus,
    load_capability_grant_set_inner, load_invocation_outcome_inner, load_invocation_status_inner,
};
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, ApprovalRequirement, AuthorityRevision, AuthorityRevisionSet,
    CapabilityDenial, CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity,
    CapabilityName, CapabilityReleaseVersion, CatalogIdentity, CheckpointRef,
    ConfirmationRequirement, ConsentRequirement, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticSeverity, DiagnosticStage, EffectClassification, EffectiveCapabilityGrant,
    EffectiveCapabilityGrantParts, EvidenceKind, ExecutionMode, InvocationId, InvocationOutcome,
    InvocationStatus, NonEmptySet, NormalizedResourceSelector, PolicyRevision,
    ProtectedEvidenceRequestRef, RequiredEvidence, RetryClass, SessionRef, Sha256Digest,
    StableCapabilityError, Suspension, TaskRef, Timestamp,
};
use pyo3::Python;

#[test]
fn grant_set_projection_exposes_only_stable_scalar_and_tuple_values() {
    let projection = PyCapabilityGrantSet::from(grant_set());

    assert_eq!(projection.admission_id(), "adm-1");
    assert_eq!(projection.actor(), "actor:alice");
    assert_eq!(projection.issued_at(), 100);
    assert_eq!(projection.expires_at(), 200);
    assert_eq!(projection.catalog_name(), "provider.test");
    assert_eq!(
        projection.authority_revisions().authority_revision_digest(),
        grant_set()
            .authority_revisions()
            .authority_revision_digest()
            .to_string()
    );
    assert!(
        projection
            .canonical_json()
            .unwrap()
            .windows(b"cases.comment".len())
            .any(|window| window == b"cases.comment")
    );
    Python::attach(|py| {
        let denials = projection.optional_denials(py).unwrap();
        assert_eq!(denials.len(), 1);
        let denial = denials.get_item(0).unwrap();
        assert_eq!(
            denial.getattr("name").unwrap().extract::<String>().unwrap(),
            "notes.read"
        );
        let diagnostic = denial.getattr("diagnostic").unwrap();
        assert_eq!(
            diagnostic
                .getattr("code")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "KF-AUTH-001"
        );
        assert_eq!(
            diagnostic
                .getattr("category")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "authorization"
        );
        assert_eq!(
            diagnostic
                .getattr("severity")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "warning"
        );
        assert_eq!(
            diagnostic
                .getattr("message")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "optional capability denied"
        );
        assert_eq!(diagnostic.getattr("details").unwrap().len().unwrap(), 0);
    });
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

#[test]
fn outcome_and_status_expose_detached_structured_values() {
    Python::attach(|py| {
        let outcome = PyInvocationOutcome::from(InvocationOutcome::Succeeded {
            invocation_id: InvocationId::new("inv-1").unwrap(),
            result: serde_json::json!({"accepted": true, "caseId": "case-1"}),
        });
        let result = outcome.result(py).unwrap().unwrap();
        assert!(
            result
                .bind(py)
                .get_item("accepted")
                .unwrap()
                .extract::<bool>()
                .unwrap()
        );

        let failed = PyInvocationOutcome::from(InvocationOutcome::Failed {
            invocation_id: InvocationId::new("inv-1").unwrap(),
            error: StableCapabilityError::try_new(
                "CASE_CONFLICT",
                "conflict",
                RetryClass::AfterRefresh,
                "case changed",
            )
            .unwrap(),
        });
        assert_eq!(failed.error().unwrap().code(), "CASE_CONFLICT");
        assert_eq!(failed.error().unwrap().retry(), "after_refresh");

        let denied = PyInvocationStatus::from(InvocationStatus::Denied {
            invocation_id: InvocationId::new("inv-1").unwrap(),
            diagnostic: Diagnostic::error(
                DiagnosticCode::InvocationDenied,
                DiagnosticCategory::Authorization,
                DiagnosticStage::Invoke,
                "invocation denied",
            ),
        });
        assert_eq!(denied.diagnostic().unwrap().code(), "KF-AUTH-003");

        let suspended = PyInvocationStatus::from(InvocationStatus::Suspended {
            invocation_id: InvocationId::new("inv-1").unwrap(),
            suspension: Suspension::try_new(
                CheckpointRef::new("checkpoint:opaque:1").unwrap(),
                EvidenceKind::Approval,
                ProtectedEvidenceRequestRef::new("evidence-request:opaque:1").unwrap(),
                Sha256Digest::from_bytes([11; 32]),
            )
            .unwrap(),
        });
        assert_eq!(
            suspended.suspension().unwrap().checkpoint_ref(),
            "checkpoint:opaque:1"
        );
    });
}

fn grant_set() -> CapabilityGrantSet {
    CapabilityGrantSet::try_new(CapabilityGrantSetParts {
        admission_id: AdmissionId::new("adm-1").unwrap(),
        admission_request_digest: Sha256Digest::from_bytes([9; 32]),
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        policy_revision: PolicyRevision::new("policy:7").unwrap(),
        catalog_identity: CatalogIdentity {
            name: "provider.test".to_owned(),
            revision: "revision-1".to_owned(),
        },
        catalog_digest: Sha256Digest::from_bytes([1; 32]),
        authority_revisions: AuthorityRevisionSet::try_new(vec![
            AuthorityRevision::try_new("policy", "7").unwrap(),
        ])
        .unwrap(),
        issued_at: Timestamp::new(100),
        expires_at: Timestamp::new(200),
        grants: vec![
            EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
                capability: CapabilityIdentity::try_new(
                    CapabilityName::new("cases.comment").unwrap(),
                    CapabilityReleaseVersion::new("1.0.0").unwrap(),
                )
                .unwrap(),
                resources: vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
                execution_modes: NonEmptySet::try_new(std::collections::BTreeSet::from([
                    ExecutionMode::Immediate,
                ]))
                .unwrap(),
                maximum_effect: EffectClassification::ReadOnly,
                expires_at: Timestamp::new(180),
                required_evidence: RequiredEvidence::new(
                    ConfirmationRequirement::None,
                    ApprovalRequirement::None,
                    ConsentRequirement::None,
                ),
                freshness: Default::default(),
                preconditions: Vec::new(),
            })
            .unwrap(),
        ],
        optional_denials: vec![
            CapabilityDenial::try_new(
                CapabilityIdentity::try_new(
                    CapabilityName::new("notes.read").unwrap(),
                    CapabilityReleaseVersion::new("1.0.0").unwrap(),
                )
                .unwrap(),
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
                    details: Default::default(),
                },
            )
            .unwrap(),
        ],
    })
    .unwrap()
}
