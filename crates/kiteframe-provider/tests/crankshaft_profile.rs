use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, AuthorityRevision, AuthorityRevisionSet, CapabilityCatalog,
    CapabilityDescriptor, CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity,
    CatalogIdentity, CheckpointRef, Diagnostic, EffectProposal, EffectiveCapabilityGrant,
    EvidenceReferences, IdempotencyKey, InvocationId, InvocationOutcome, InvocationRequest,
    LockedCapability, NormalizedResourceSelector, PolicyRevision, ProtectedEvidenceRequestRef,
    RetryClass, SessionRef, Sha256Digest, StatusRequest, Suspension, TaskRef, Timestamp,
    TraceContext,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AuthorizationBackend,
    AuthorizationDecision, DecisionRef, IdempotencyScopeValue, InvocationAuthorizationRequest,
    InvocationReservationInput, InvocationState, InvocationStatusContext, InvocationStore,
    InvocationStoreClock, InvocationTransition, NarrowedAuthorizationConditions,
    PortableInvocationRefs, SafeDenialReason, StatusState, TransitionAuditRecord,
    VerifiedHumanPrincipal, VerifiedWorkloadPrincipal, correlate_principals,
    require_current_authorization,
};
use kiteframe_provider_sqlite::SqliteInvocationStore;
use serde::Deserialize;
use serde_json::Value;

const TRACEPARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";

#[tokio::test]
async fn workforce_profile_preserves_dual_principals_and_projection_scope() {
    let catalog = load_catalog();
    let admission: AdmissionFixture = load_fixture("admission.json");
    let result: Value = load_fixture("read-result.json");

    assert_eq!(catalog.descriptors().len(), 2);
    assert_eq!(catalog.catalog_digest(), &admission.catalog_digest);
    for descriptor in catalog.descriptors() {
        assert!(descriptor.stable_errors().len() >= 2);
        assert!(!descriptor.execution_modes().as_set().is_empty());
        assert!(!descriptor.input_schema().as_value().is_null());
        assert!(!descriptor.output_schema().as_value().is_null());
        assert_eq!(
            descriptor.descriptor_digest(),
            admission
                .descriptor_digests
                .get(descriptor.identity().name().as_str())
                .expect("fixture declares every descriptor digest")
        );
    }

    let revisions = revision_set(&admission.authority_revisions);
    assert_eq!(
        revisions.authority_revision_digest(),
        &admission.authority_revision_digest
    );
    let grant_set = CapabilityGrantSet::try_new(CapabilityGrantSetParts {
        admission_id: AdmissionId::new(&admission.principals.admission_ref).unwrap(),
        admission_request_digest: admission.admission_request_digest,
        delegation_ancestry_digest: admission.delegation_ancestry_digest,
        actor: ActorRef::new(&admission.principals.actor_ref).unwrap(),
        agent: AgentRef::new(&admission.principals.agent_ref).unwrap(),
        task: TaskRef::new(&admission.principals.task_ref).unwrap(),
        session: SessionRef::new(&admission.principals.session_ref).unwrap(),
        policy_revision: admission.policy_revision.clone(),
        catalog_identity: catalog.identity().clone(),
        catalog_digest: *catalog.catalog_digest(),
        authority_revisions: revisions.clone(),
        issued_at: Timestamp::new(admission.issued_at),
        expires_at: Timestamp::new(admission.expires_at),
        grants: admission.grants.clone(),
        optional_denials: vec![],
    })
    .unwrap();
    assert_eq!(grant_set.grant_digest(), &admission.grant_digest);
    assert_eq!(
        grant_set
            .grants()
            .iter()
            .find(|grant| grant.capability().name().as_str() == "workforce.absence.read")
            .unwrap()
            .expires_at(),
        Timestamp::new(800)
    );
    assert_eq!(
        grant_set
            .grants()
            .iter()
            .find(|grant| grant.capability().name().as_str() == "workforce.absence.propose")
            .unwrap()
            .expires_at(),
        Timestamp::new(850)
    );
    assert!(grant_set.grants().iter().all(|grant| {
        grant.resources().iter().all(|resource| {
            resource
                .as_str()
                .starts_with("tenant:tenant-1/employee:employee-7")
        })
    }));

    let principals = correlate(&admission.principals);
    assert_eq!(principals.tenant_ref().as_str(), "tenant-1");
    assert_eq!(principals.human_ref().as_str(), "employee-7");
    assert_eq!(principals.workload_ref().as_str(), "workforce-harness-2");
    assert_eq!(principals.run_ref().as_str(), "run-9");

    let read = descriptor(&catalog, "workforce.absence.read");
    let locked_read = LockedCapability::try_new(
        read.identity().clone(),
        read.clone(),
        *read.descriptor_digest(),
        digest(31),
        digest(32),
        digest(33),
        digest(34),
    )
    .unwrap();
    locked_read.descriptor().validate_output(&result).unwrap();
    assert_eq!(
        serde_json_canonicalizer::to_vec(&result).unwrap(),
        br#"{"employeeId":"employee-7","status":"approved"}"#
    );
    assert!(result.get("salary").is_none());
    assert!(result.get("coworker").is_none());
}

#[tokio::test]
async fn revision_change_revokes_then_restart_status_recovers_effect() {
    let catalog = load_catalog();
    let admission: AdmissionFixture = load_fixture("admission.json");
    let policies: PolicyRevisionFixture = load_fixture("policy-revisions.json");
    let effects: EffectFixture = load_fixture("effect-outcomes.json");
    assert_eq!(
        effects.coverage,
        vec![
            "allow",
            "revocation",
            "proposal_bound_suspension",
            "policy_revision_change",
            "outcome_unknown",
            "traced_status_first_lookup",
            "restart_recovery",
        ]
    );

    let principals = correlate(&admission.principals);
    let admitted_revisions = revision_set(&policies.admitted);
    let current_revisions = revision_set(&policies.current);
    let backend = RevisionChangedBackend {
        allowed_at: admitted_revisions.clone(),
        current: current_revisions,
    };
    let authorization_request = InvocationAuthorizationRequest::new(
        principals,
        capability("workforce.absence.propose"),
        NormalizedResourceSelector::new(&effects.resource).unwrap(),
        admission.grant_digest,
        admitted_revisions,
    );
    let allowed = require_current_authorization(
        &RevisionChangedBackend {
            allowed_at: revision_set(&policies.admitted),
            current: revision_set(&policies.admitted),
        },
        &authorization_request,
    )
    .await
    .unwrap();
    assert!(matches!(allowed, AuthorizationDecision::Allow { .. }));
    let denied = require_current_authorization(&backend, &authorization_request)
        .await
        .unwrap_err();
    assert_eq!(denied.code.as_str(), "KF-AUTH-004");
    let revoked = require_current_authorization(
        &RevokedBackend {
            current: revision_set(&policies.current),
        },
        &authorization_request,
    )
    .await
    .unwrap_err();
    assert_eq!(revoked.code.as_str(), "KF-AUTH-003");

    let effect_descriptor = descriptor(&catalog, "workforce.absence.propose");
    let invocation = InvocationRequest::try_new(
        InvocationId::new(&effects.invocation_id).unwrap(),
        AdmissionId::new(&admission.principals.admission_ref).unwrap(),
        admission.grant_digest,
        admission.delegation_ancestry_digest,
        capability("workforce.absence.propose"),
        &effects.resource,
        effects.arguments.clone(),
        BTreeMap::new(),
        Some(effects.idempotency_key.clone()),
        EvidenceReferences::try_new(BTreeMap::new()).unwrap(),
        trace_context(TRACEPARENT),
    )
    .unwrap();
    let proposal = EffectProposal::try_new(&invocation, effect_descriptor).unwrap();
    let suspension = Suspension::try_new(
        CheckpointRef::new(&effects.checkpoint_ref).unwrap(),
        effects.evidence_kind,
        ProtectedEvidenceRequestRef::new(&effects.evidence_request_ref).unwrap(),
        *proposal.proposal_digest(),
    )
    .unwrap();
    assert_eq!(suspension.proposal_digest(), proposal.proposal_digest());
    InvocationOutcome::Suspended {
        invocation_id: InvocationId::new(&effects.invocation_id).unwrap(),
        suspension: suspension.clone(),
    }
    .validate_against(&invocation, effect_descriptor)
    .unwrap();
    let wrong_binding = InvocationOutcome::Suspended {
        invocation_id: InvocationId::new(&effects.invocation_id).unwrap(),
        suspension: Suspension::try_new(
            CheckpointRef::new(&effects.checkpoint_ref).unwrap(),
            effects.evidence_kind,
            ProtectedEvidenceRequestRef::new(&effects.evidence_request_ref).unwrap(),
            digest(99),
        )
        .unwrap(),
    }
    .validate_against(&invocation, effect_descriptor)
    .unwrap_err();
    assert_eq!(wrong_binding.code.as_str(), "KF-CAP-002");

    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("workforce-profile.sqlite3");
    let store = SqliteInvocationStore::open_with_clock(
        &database_path,
        Arc::new(FixedInvocationStoreClock(Timestamp::new(500))),
    )
    .await
    .unwrap();
    store
        .reserve_or_get(
            InvocationReservationInput {
                invocation_id: InvocationId::new(&effects.invocation_id).unwrap(),
                status_id: effects.status_id.clone(),
                scope: IdempotencyScopeValue::try_new(
                    ActorRef::new(&admission.principals.actor_ref).unwrap(),
                    capability("workforce.absence.propose"),
                    NormalizedResourceSelector::new(&effects.resource).unwrap(),
                    "workforce.absence.propose",
                )
                .unwrap(),
                idempotency_key: IdempotencyKey::new(&effects.idempotency_key).unwrap(),
                request_digest: effects.request_digest,
                admission_id: AdmissionId::new(&admission.principals.admission_ref).unwrap(),
                grant_digest: admission.grant_digest,
                catalog_identity: catalog.identity().clone(),
                catalog_digest: *catalog.catalog_digest(),
                authority_revision_digest: admission.authority_revision_digest,
                status_context: status_context(&admission.principals),
                proposal_digest: *proposal.proposal_digest(),
                protected_evidence_refs: vec![
                    ProtectedEvidenceRequestRef::new(&effects.evidence_request_ref).unwrap(),
                ],
            },
            effect_descriptor,
            Timestamp::new(effects.retention_until),
        )
        .await
        .unwrap();
    let invocation_id = InvocationId::new(&effects.invocation_id).unwrap();
    store
        .transition(
            &invocation_id,
            InvocationTransition::try_new(
                InvocationState::Reserved,
                InvocationState::Pending,
                TransitionAuditRecord::Authorization(effects.authorization_audit_ref.clone()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .transition(
            &invocation_id,
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::OutcomeUnknown,
                TransitionAuditRecord::None,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    drop(store);

    let restarted_service_store = SqliteInvocationStore::open_with_clock(
        &database_path,
        Arc::new(FixedInvocationStoreClock(Timestamp::new(501))),
    )
    .await
    .unwrap();
    let status_request =
        StatusRequest::new(invocation_id, trace_context(&effects.status_traceparent));
    assert_eq!(
        status_request.trace_context().traceparent(),
        effects.status_traceparent
    );
    let status = restarted_service_store
        .status(&status_request, &status_context(&admission.principals))
        .await
        .unwrap();
    assert_eq!(status.status_state(), StatusState::OutcomeUnknown);
    assert_eq!(status.proposal_digest(), proposal.proposal_digest());
    assert_eq!(
        status.audit_authorization_record_id(),
        Some(effects.authorization_audit_ref.as_str())
    );
    assert_eq!(
        restarted_service_store.last_traceparent().as_deref(),
        Some(effects.status_traceparent.as_str())
    );
    let portable_unknown = InvocationOutcome::outcome_unknown(
        InvocationId::new(&effects.invocation_id).unwrap(),
        Diagnostic::outcome_unknown("look up invocation status before retry"),
    )
    .unwrap();
    assert_eq!(
        portable_unknown.diagnostic().unwrap().retry,
        RetryClass::StatusFirst
    );
}

struct FixedInvocationStoreClock(Timestamp);

impl InvocationStoreClock for FixedInvocationStoreClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdmissionFixture {
    principals: PrincipalFixture,
    admission_request_digest: Sha256Digest,
    delegation_ancestry_digest: Sha256Digest,
    catalog_digest: Sha256Digest,
    descriptor_digests: BTreeMap<String, Sha256Digest>,
    policy_revision: PolicyRevision,
    authority_revisions: Vec<AuthorityRevision>,
    authority_revision_digest: Sha256Digest,
    issued_at: u64,
    expires_at: u64,
    grants: Vec<EffectiveCapabilityGrant>,
    grant_digest: Sha256Digest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrincipalFixture {
    tenant_ref: String,
    human_ref: String,
    workload_ref: String,
    run_ref: String,
    actor_ref: String,
    agent_ref: String,
    task_ref: String,
    session_ref: String,
    admission_ref: String,
    expires_at: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PolicyRevisionFixture {
    admitted: Vec<AuthorityRevision>,
    current: Vec<AuthorityRevision>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EffectFixture {
    coverage: Vec<String>,
    invocation_id: String,
    status_id: String,
    resource: String,
    arguments: Value,
    idempotency_key: String,
    checkpoint_ref: String,
    evidence_kind: kiteframe_contract::EvidenceKind,
    evidence_request_ref: String,
    status_traceparent: String,
    retention_until: u64,
    request_digest: Sha256Digest,
    authorization_audit_ref: String,
}

struct RevisionChangedBackend {
    allowed_at: AuthorityRevisionSet,
    current: AuthorityRevisionSet,
}

struct RevokedBackend {
    current: AuthorityRevisionSet,
}

#[async_trait]
impl AuthorizationBackend for RevokedBackend {
    async fn list_admissible(
        &self,
        _request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, kiteframe_contract::Diagnostic> {
        unreachable!("profile only exercises invocation-time authorization")
    }

    async fn check(
        &self,
        _request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, kiteframe_contract::Diagnostic> {
        Ok(AuthorizationDecision::Deny {
            reason: SafeDenialReason::ResourceDenied,
            decision_ref: DecisionRef::new("decision-workforce-revoked").unwrap(),
        })
    }

    async fn revisions(&self) -> Result<AuthorityRevisionSet, kiteframe_contract::Diagnostic> {
        Ok(self.current.clone())
    }
}

#[async_trait]
impl AuthorizationBackend for RevisionChangedBackend {
    async fn list_admissible(
        &self,
        _request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, kiteframe_contract::Diagnostic> {
        unreachable!("profile only exercises invocation-time authorization")
    }

    async fn check(
        &self,
        request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, kiteframe_contract::Diagnostic> {
        Ok(AuthorizationDecision::Allow {
            decision_ref: DecisionRef::new("decision-workforce-allow").unwrap(),
            authority_revisions: self.allowed_at.clone(),
            decided_at: Timestamp::new(500),
            narrowed_conditions: NarrowedAuthorizationConditions::new(
                vec![request.selected_resource().clone()],
                Timestamp::new(900),
                vec![],
            )
            .unwrap(),
        })
    }

    async fn revisions(&self) -> Result<AuthorityRevisionSet, kiteframe_contract::Diagnostic> {
        Ok(self.current.clone())
    }
}

fn load_catalog() -> CapabilityCatalog {
    load_fixture("catalog.json")
}

fn descriptor<'a>(catalog: &'a CapabilityCatalog, name: &str) -> &'a CapabilityDescriptor {
    catalog
        .descriptors()
        .iter()
        .find(|descriptor| descriptor.identity().name().as_str() == name)
        .unwrap()
}

fn correlate(fixture: &PrincipalFixture) -> kiteframe_provider::AuthenticatedInvocationContext {
    correlate_principals(
        VerifiedHumanPrincipal::try_new(
            &fixture.tenant_ref,
            &fixture.human_ref,
            ActorRef::new(&fixture.actor_ref).unwrap(),
            Timestamp::new(fixture.expires_at),
        )
        .unwrap(),
        VerifiedWorkloadPrincipal::try_new(
            &fixture.tenant_ref,
            &fixture.workload_ref,
            &fixture.run_ref,
            AgentRef::new(&fixture.agent_ref).unwrap(),
            TaskRef::new(&fixture.task_ref).unwrap(),
            SessionRef::new(&fixture.session_ref).unwrap(),
            AdmissionId::new(&fixture.admission_ref).unwrap(),
            Timestamp::new(fixture.expires_at),
        )
        .unwrap(),
        PortableInvocationRefs::new(
            ActorRef::new(&fixture.actor_ref).unwrap(),
            AgentRef::new(&fixture.agent_ref).unwrap(),
            kiteframe_provider::RunRef::new(&fixture.run_ref).unwrap(),
            TaskRef::new(&fixture.task_ref).unwrap(),
            SessionRef::new(&fixture.session_ref).unwrap(),
            AdmissionId::new(&fixture.admission_ref).unwrap(),
            Timestamp::new(400),
        ),
    )
    .unwrap()
}

fn capability(name: &str) -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        kiteframe_contract::CapabilityName::new(name).unwrap(),
        kiteframe_contract::CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn revision_set(entries: &[AuthorityRevision]) -> AuthorityRevisionSet {
    AuthorityRevisionSet::try_new(entries.to_vec()).unwrap()
}

fn status_context(fixture: &PrincipalFixture) -> InvocationStatusContext {
    InvocationStatusContext::try_new(
        &fixture.tenant_ref,
        &fixture.human_ref,
        &fixture.workload_ref,
        &fixture.run_ref,
        &fixture.actor_ref,
        &fixture.agent_ref,
        &fixture.task_ref,
        &fixture.session_ref,
        &fixture.admission_ref,
    )
    .unwrap()
}

fn trace_context(traceparent: &str) -> TraceContext {
    TraceContext::try_new(traceparent, None, BTreeMap::new()).unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}

fn load_fixture<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = fixture_dir().join(name);
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/provider/fixtures/crankshaft-profile")
}
