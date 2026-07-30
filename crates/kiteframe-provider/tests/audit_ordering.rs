use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, AuthorityRevision, AuthorityRevisionSet, CapabilityDescriptor,
    CapabilityDescriptorParts, CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity,
    CapabilityName, CapabilityReleaseVersion, ConfirmationRequirement, ConsentRequirement,
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, EffectClassification,
    EffectProposal, EffectiveCapabilityGrant, EffectiveCapabilityGrantParts, EvidenceReferences,
    ExecutionMode, FreshnessRequirement, IdempotencyRequirement, IdempotencyScope, InvocationId,
    InvocationOutcome, InvocationRequest, LockedCapability, NonEmptySet,
    NormalizedResourceSelector, RequiredEvidence, ResourceSelectorSchema, SessionRef, Sha256Digest,
    TaskRef, Timestamp, TraceContext,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AuditRecord, AuditSink,
    AuthorizationDecision, CapabilityOperation, DurableAuditReceipt, EffectAuditDigests,
    EffectEnforcementPlane, InMemoryInvocationAdmissionStore, InMemoryInvocationStore,
    InvocationAdmission, InvocationAuthorizationRequest, InvocationCheckpointIssuer,
    InvocationClock, InvocationContext, InvocationEventSink, InvocationEvidenceProvider,
    InvocationService, InvocationStatusContext, InvocationStore, InvocationStoreClock,
    NarrowedAuthorizationConditions, OperationFailure, OperationRegistry, Precondition,
    ProviderPrincipalVerifier, StatusState, VerifiedEvidence, VerifiedHumanPrincipal,
    VerifiedProviderPrincipals, VerifiedWorkloadPrincipal,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn write_ahead_receipt_precedes_effect() {
    let fixture = fixture(None);

    let outcome = fixture.service.invoke(fixture.request()).await.unwrap();

    assert!(matches!(outcome, InvocationOutcome::Succeeded { .. }));
    assert_eq!(
        enforcement_events(&fixture.events.snapshot()),
        [
            "authorize",
            "reserve",
            "audit_authorization",
            "execute",
            "audit_outcome",
            "terminal_status",
        ]
    );
    assert_eq!(fixture.effect_count.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.audit.records().len(), 2);
}

#[tokio::test]
async fn audit_outage_blocks_effect() {
    let fixture = fixture(Some(1));

    let error = fixture.service.invoke(fixture.request()).await.unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUDIT-001");
    assert_eq!(fixture.effect_count.load(Ordering::SeqCst), 0);
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn outcome_append_failure_marks_status_unknown() {
    let fixture = fixture(Some(2));
    let request = fixture.request();

    let outcome = fixture.service.invoke(request.clone()).await.unwrap();

    assert!(matches!(outcome, InvocationOutcome::OutcomeUnknown { .. }));
    assert_eq!(fixture.effect_count.load(Ordering::SeqCst), 1);
    let status = fixture
        .store
        .status(
            &kiteframe_contract::StatusRequest::new(
                request.invocation_id().clone(),
                request.trace_context().clone(),
            ),
            &InvocationStatusContext::try_new(
                "tenant-1",
                "human-7",
                "workload-2",
                "run-9",
                "actor-7",
                "agent-2",
                "task-4",
                "session-3",
                "admission-audit-5",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status_state(), StatusState::OutcomeUnknown);
    assert!(status.audit_authorization_record_id().is_some());
    assert!(status.audit_outcome_record_id().is_none());
    assert_eq!(
        enforcement_events(&fixture.events.snapshot()),
        [
            "authorize",
            "reserve",
            "audit_authorization",
            "execute",
            "terminal_status",
        ]
    );
}

fn enforcement_events(events: &[&'static str]) -> Vec<&'static str> {
    events
        .iter()
        .copied()
        .filter(|event| {
            matches!(
                *event,
                "authorize"
                    | "reserve"
                    | "audit_authorization"
                    | "execute"
                    | "audit_outcome"
                    | "terminal_status"
            )
        })
        .collect()
}

struct Fixture {
    service: InvocationService,
    events: RecordingEvents,
    audit: Arc<RecordingAudit>,
    store: Arc<InMemoryInvocationStore>,
    effect_count: Arc<AtomicU64>,
    admission_id: AdmissionId,
    grant_digest: Sha256Digest,
    identity: CapabilityIdentity,
}

impl Fixture {
    fn request(&self) -> InvocationRequest {
        InvocationRequest::try_new(
            InvocationId::new("invocation-audit-7").unwrap(),
            self.admission_id.clone(),
            self.grant_digest,
            digest(6),
            self.identity.clone(),
            "case:42",
            json!({"caseId": "42"}),
            BTreeMap::new(),
            Some("idempotency-audit-7".to_owned()),
            EvidenceReferences::default(),
            trace_context(),
        )
        .unwrap()
    }
}

fn fixture(fail_append: Option<u64>) -> Fixture {
    let identity = CapabilityIdentity::try_new(
        CapabilityName::new("cases.update").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap();
    let descriptor = descriptor(identity.clone());
    let [
        input_digest,
        output_digest,
        stable_errors_digest,
        safety_digest,
    ] = descriptor_part_digests(&descriptor);
    let locked = LockedCapability::try_new(
        identity.clone(),
        descriptor.clone(),
        *descriptor.descriptor_digest(),
        input_digest,
        output_digest,
        stable_errors_digest,
        safety_digest,
    )
    .unwrap();
    let grant = EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: identity.clone(),
        resources: vec![selector("case:42")],
        execution_modes: modes(&[ExecutionMode::Deferred]),
        maximum_effect: EffectClassification::ReversibleWrite,
        expires_at: Timestamp::new(500),
        required_evidence: RequiredEvidence::new(
            ConfirmationRequirement::None,
            kiteframe_contract::ApprovalRequirement::None,
            ConsentRequirement::None,
        ),
        freshness: FreshnessRequirement {
            max_admission_age_seconds: None,
            policy_revision_required: true,
            max_input_age_seconds: None,
        },
        preconditions: vec![],
    })
    .unwrap();
    let admission_id = AdmissionId::new("admission-audit-5").unwrap();
    let grant_set = CapabilityGrantSet::try_new(CapabilityGrantSetParts {
        admission_id: admission_id.clone(),
        admission_request_digest: digest(1),
        delegation_ancestry_digest: digest(6),
        actor: ActorRef::new("actor-7").unwrap(),
        agent: AgentRef::new("agent-2").unwrap(),
        task: TaskRef::new("task-4").unwrap(),
        session: SessionRef::new("session-3").unwrap(),
        policy_revision: kiteframe_contract::PolicyRevision::new("policy-r1").unwrap(),
        catalog_identity: kiteframe_contract::CatalogIdentity {
            name: "provider.catalog".to_owned(),
            revision: "1.0.0".to_owned(),
        },
        catalog_digest: digest(10),
        authority_revisions: revisions(),
        issued_at: Timestamp::new(100),
        expires_at: Timestamp::new(550),
        grants: vec![grant],
        optional_denials: vec![],
    })
    .unwrap();
    let grant_digest = *grant_set.grant_digest();
    let admission = InvocationAdmission::try_new(grant_set, vec![locked]).unwrap();
    let admissions = Arc::new(InMemoryInvocationAdmissionStore::new(vec![admission]).unwrap());
    let effect_count = Arc::new(AtomicU64::new(0));
    let mut registry = OperationRegistry::new();
    registry
        .register(CountingOperation {
            identity: identity.clone(),
            effect_count: effect_count.clone(),
        })
        .unwrap();
    let registry = registry.freeze(Arc::new(AllowAuthorization)).unwrap();
    let events = RecordingEvents::new();
    let audit = Arc::new(RecordingAudit::new(fail_append));
    let store = Arc::new(InMemoryInvocationStore::with_clock(Arc::new(FixedClock)));
    let enforcement = EffectEnforcementPlane::new(
        store.clone(),
        audit.clone(),
        EffectAuditDigests::new(digest(21), digest(22), digest(23), digest(24)),
    );
    let service = InvocationService::try_new(
        admissions,
        Arc::new(FixedPrincipalVerifier {
            admission_id: admission_id.clone(),
        }),
        registry,
        Arc::new(NoEvidence),
        Arc::new(FixedClock),
        Arc::new(NoCheckpoint),
    )
    .unwrap()
    .with_event_sink(Arc::new(events.clone()))
    .with_effect_enforcement(enforcement);
    Fixture {
        service,
        events,
        audit,
        store,
        effect_count,
        admission_id,
        grant_digest,
        identity,
    }
}

#[derive(Clone)]
struct RecordingEvents(Arc<Mutex<Vec<&'static str>>>);

impl RecordingEvents {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn snapshot(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

impl InvocationEventSink for RecordingEvents {
    fn record(&self, event: &'static str) {
        self.0.lock().unwrap().push(event);
    }
}

struct RecordingAudit {
    calls: AtomicU64,
    fail_append: Option<u64>,
    records: Mutex<Vec<AuditRecord>>,
}

impl RecordingAudit {
    fn new(fail_append: Option<u64>) -> Self {
        Self {
            calls: AtomicU64::new(0),
            fail_append,
            records: Mutex::new(Vec::new()),
        }
    }

    fn records(&self) -> Vec<AuditRecord> {
        self.records.lock().unwrap().clone()
    }
}

#[async_trait]
impl AuditSink for RecordingAudit {
    async fn append(&self, record: AuditRecord) -> Result<DurableAuditReceipt, Diagnostic> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_append == Some(call) {
            return Err(Diagnostic::error(
                DiagnosticCode::AuditUnavailable,
                DiagnosticCategory::Audit,
                DiagnosticStage::Audit,
                "audit ledger is unavailable",
            ));
        }
        self.records.lock().unwrap().push(record);
        DurableAuditReceipt::try_new(
            "tenant-1",
            call,
            digest((call - 1) as u8),
            digest(call as u8),
        )
        .map_err(|message| {
            Diagnostic::error(
                DiagnosticCode::AuditUnavailable,
                DiagnosticCategory::Audit,
                DiagnosticStage::Audit,
                message,
            )
        })
    }
}

struct FixedClock;

impl InvocationClock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::new(200)
    }
}

impl InvocationStoreClock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::new(200)
    }
}

struct NoCheckpoint;

impl InvocationCheckpointIssuer for NoCheckpoint {
    fn issue(
        &self,
        _proposal: &EffectProposal,
    ) -> Result<kiteframe_contract::CheckpointRef, Diagnostic> {
        unreachable!("the fixture has no evidence gate")
    }
}

struct NoEvidence;

#[async_trait]
impl InvocationEvidenceProvider for NoEvidence {
    async fn resolve(
        &self,
        _reference: &kiteframe_contract::ProtectedEvidenceRequestRef,
    ) -> Result<VerifiedEvidence, Diagnostic> {
        unreachable!("the fixture has no evidence references")
    }
}

struct FixedPrincipalVerifier {
    admission_id: AdmissionId,
}

#[async_trait]
impl ProviderPrincipalVerifier for FixedPrincipalVerifier {
    async fn verify(&self) -> Result<VerifiedProviderPrincipals, Diagnostic> {
        Ok(VerifiedProviderPrincipals::new(
            VerifiedHumanPrincipal::try_new(
                "tenant-1",
                "human-7",
                ActorRef::new("actor-7").unwrap(),
                Timestamp::new(600),
            )
            .unwrap(),
            VerifiedWorkloadPrincipal::try_new(
                "tenant-1",
                "workload-2",
                "run-9",
                AgentRef::new("agent-2").unwrap(),
                TaskRef::new("task-4").unwrap(),
                SessionRef::new("session-3").unwrap(),
                self.admission_id.clone(),
                Timestamp::new(600),
            )
            .unwrap(),
        ))
    }
}

struct AllowAuthorization;

#[async_trait]
impl kiteframe_provider::AuthorizationBackend for AllowAuthorization {
    async fn list_admissible(
        &self,
        request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, Diagnostic> {
        Ok(AdmissionAuthorizationResult::new(vec![
            request.capability().clone(),
        ]))
    }

    async fn check(
        &self,
        request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, Diagnostic> {
        AuthorizationDecision::allow(
            "decision-audit-allow",
            revisions(),
            Timestamp::new(200),
            NarrowedAuthorizationConditions::new(
                vec![request.selected_resource().clone()],
                Timestamp::new(400),
                vec![],
            )
            .unwrap(),
        )
        .map_err(|message| {
            Diagnostic::error(
                DiagnosticCode::InvocationDenied,
                DiagnosticCategory::Authorization,
                DiagnosticStage::Invoke,
                message,
            )
        })
    }

    async fn revisions(&self) -> Result<AuthorityRevisionSet, Diagnostic> {
        Ok(revisions())
    }
}

struct CountingOperation {
    identity: CapabilityIdentity,
    effect_count: Arc<AtomicU64>,
}

#[async_trait]
impl CapabilityOperation for CountingOperation {
    fn identity(&self) -> &CapabilityIdentity {
        &self.identity
    }

    async fn validate_preconditions(
        &self,
        _context: &InvocationContext,
        _preconditions: &[Precondition],
    ) -> Result<(), Diagnostic> {
        Ok(())
    }

    async fn execute(
        &self,
        _context: &InvocationContext,
        _arguments: Value,
    ) -> Result<Value, OperationFailure> {
        self.effect_count.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"changed": true}))
    }
}

fn descriptor(identity: CapabilityIdentity) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity,
        summary: "Update a stable case projection".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId"],
            "properties": {"caseId": {"type": "string"}},
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["changed"],
            "properties": {"changed": {"type": "boolean"}},
        }),
        stable_errors: vec![],
        execution_modes: modes(&[ExecutionMode::Deferred]),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({
            "type": "string",
            "pattern": "^case:[A-Za-z0-9-]+$"
        }))
        .unwrap(),
        effect: EffectClassification::ReversibleWrite,
        idempotency: IdempotencyRequirement::Required {
            scope: IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: std::num::NonZeroU64::new(3600).unwrap(),
        },
        freshness: FreshnessRequirement {
            max_admission_age_seconds: None,
            policy_revision_required: true,
            max_input_age_seconds: None,
        },
        preconditions: vec![],
        confirmation: ConfirmationRequirement::None,
        approval: kiteframe_contract::ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn selector(value: &str) -> NormalizedResourceSelector {
    NormalizedResourceSelector::new(value).unwrap()
}

fn modes(values: &[ExecutionMode]) -> NonEmptySet<ExecutionMode> {
    NonEmptySet::try_new(values.iter().copied().collect::<BTreeSet<_>>()).unwrap()
}

fn revisions() -> AuthorityRevisionSet {
    AuthorityRevisionSet::try_new(vec![AuthorityRevision::try_new("policy", "r1").unwrap()])
        .unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}

fn trace_context() -> TraceContext {
    TraceContext::try_new(
        "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
        None,
        BTreeMap::new(),
    )
    .unwrap()
}

fn descriptor_part_digests(descriptor: &CapabilityDescriptor) -> [Sha256Digest; 4] {
    let wire = serde_json::to_value(descriptor).unwrap();
    let object = wire.as_object().unwrap();
    let mut safety = serde_json::Map::new();
    for name in [
        "executionModes",
        "resourceSelectorSchema",
        "effect",
        "idempotency",
        "freshness",
        "preconditions",
        "confirmation",
        "approval",
        "consent",
    ] {
        safety.insert(name.to_owned(), object[name].clone());
    }
    [
        descriptor_part_digest("input-schema", &object["inputSchema"]),
        descriptor_part_digest("output-schema", &object["outputSchema"]),
        descriptor_part_digest("stable-errors", &object["stableErrors"]),
        descriptor_part_digest("safety-metadata", &Value::Object(safety)),
    ]
}

fn descriptor_part_digest(domain: &str, value: &Value) -> Sha256Digest {
    let canonical = serde_json_canonicalizer::to_vec(value).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(b"kiteframe.dev/capability-descriptor/");
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(canonical);
    Sha256Digest::from_bytes(hasher.finalize().into())
}
