use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use kiteframe_audit::FileAuditLedger;
use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    AuthorityRevision, AuthorityRevisionSet, CapabilityCatalog, CapabilityDescriptor,
    CapabilityDescriptorParts, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CatalogIdentity, ConfirmationRequirement, ConsentRequirement, DelegationAncestry, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, EffectClassification,
    EffectiveCapabilityGrant, EffectiveCapabilityGrantParts, EvidenceReferences, ExecutionMode,
    FreshnessRequirement, IdempotencyRequirement, IdempotencyScope, InvocationId,
    InvocationOutcome, InvocationRequest, InvocationStatus, LockedCapability, NonEmptySet,
    NormalizedResourceSelector, PolicyRevision, PreconditionDescriptor, RequestedCapability,
    RequiredEvidence, ResolvedCapabilityRequirement, ResourceSelectorSchema, SessionRef,
    Sha256Digest, TaskRef, Timestamp, TraceContext,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AdmissionService,
    AdmissionServiceConfig, AuditSink, AuthenticatedInvocationContext, AuthorityDomain,
    AuthorityPlane, AuthoritySource, AuthorityTerm, AuthorizationBackend, AuthorizationDecision,
    CapabilityOperation, EffectAuditDigests, EffectEnforcementPlane,
    InvocationAuthorizationRequest, InvocationCheckpointIssuer, InvocationClock, InvocationContext,
    InvocationEvidenceProvider, InvocationService, InvocationStoreClock,
    NarrowedAuthorizationConditions, OperationFailure, OperationRegistry, PortableInvocationRefs,
    Precondition, ProviderPrincipalVerifier as CorePrincipalVerifier, SafeDenialReason,
    VerifiedEvidence, VerifiedHumanPrincipal, VerifiedProviderPrincipals,
    VerifiedWorkloadPrincipal, correlate_principals,
};
use kiteframe_provider_http::{
    EnforcedAdmissionPlane, EnforcedInvocationPlane, ProviderHttpError, ProviderHttpServices,
    ProviderHttpState, ProviderPrincipalVerifier, ProviderRequestContext,
    VerifiedHumanAuthentication, VerifiedWorkloadAuthentication, provider_router,
};
use kiteframe_provider_sqlite::SqliteInvocationStore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const TRACEPARENT: &str = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
const RESOURCE: &str = "tenant:tenant-1/employee:employee-7";

#[tokio::test]
async fn authenticated_facade_runs_admission_effect_audit_restart_and_status() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("invocations.sqlite3");
    let ledger_root = directory.path().join("audit");
    let clock = Arc::new(FixedClock(Timestamp::new(100)));
    let store = Arc::new(
        SqliteInvocationStore::open_with_clock(&database, clock.clone())
            .await
            .unwrap(),
    );
    let ledger = Arc::new(FileAuditLedger::open(&ledger_root).unwrap());
    let backend = Arc::new(ProfileAuthorization::new(revisions("r1")));
    let catalog = catalog();
    let locked = locked_capability(catalog.descriptors()[0].clone());
    let admission = Arc::new(
        AdmissionService::try_new(
            catalog.clone(),
            vec![locked.clone()],
            authority_sources(&locked),
            AdmissionServiceConfig {
                issued_at: Timestamp::new(50),
                expires_at: Timestamp::new(500),
                policy_revision: PolicyRevision::new("policy-r1").unwrap(),
            },
        )
        .unwrap(),
    );
    let current_admission = Arc::new(Mutex::new(AdmissionId::new("bootstrap").unwrap()));
    let mut registry = OperationRegistry::new();
    registry.register(ProfileOperation).unwrap();
    let registry = registry.freeze(backend.clone()).unwrap();
    let invocation = Arc::new(
        InvocationService::try_new(
            admission.clone(),
            Arc::new(ProfileCoreVerifier {
                admission: current_admission.clone(),
            }),
            registry,
            Arc::new(UnusedEvidence),
            clock.clone(),
            Arc::new(UnusedCheckpointIssuer),
        )
        .unwrap()
        .with_effect_enforcement(EffectEnforcementPlane::new(
            store.clone(),
            ledger.clone() as Arc<dyn AuditSink>,
            EffectAuditDigests::new(digest(41), digest(42), digest(43), digest(44)),
        )),
    );
    let services = Arc::new(ProfileServices {
        catalog: catalog.clone(),
    });
    let http_verifier = Arc::new(ProfileHttpVerifier {
        admission: current_admission.clone(),
    });
    let app = provider_router(
        ProviderHttpState::new(services.clone(), store.clone())
            .with_admission_plane(Arc::new(EnforcedAdmissionPlane::new(
                admission,
                backend.clone(),
            )))
            .with_invocation_plane(Arc::new(EnforcedInvocationPlane::new(invocation))),
        http_verifier.clone(),
    );

    let admission_response = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/capability-admissions",
            Some(serde_json::to_value(admission_request(&catalog, locked)).unwrap()),
        ))
        .await
        .unwrap();
    assert_eq!(admission_response.status(), StatusCode::OK);
    let grant_set: kiteframe_contract::CapabilityGrantSet =
        serde_json::from_slice(&body(admission_response).await).unwrap();
    assert_eq!(grant_set.authority_revisions(), &revisions("r1"));
    assert_eq!(grant_set.grants().len(), 1);
    *current_admission.lock().unwrap() = grant_set.admission_id().clone();

    let first_invocation = invocation_request(&grant_set, "inv-profile-1", "effect-key-1");
    let invocation_response = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/capability-invocations/workforce.absence.propose",
            Some(serde_json::to_value(&first_invocation).unwrap()),
        ))
        .await
        .unwrap();
    assert_eq!(invocation_response.status(), StatusCode::OK);
    let outcome: InvocationOutcome =
        serde_json::from_slice(&body(invocation_response).await).unwrap();
    assert!(matches!(outcome, InvocationOutcome::Succeeded { .. }));

    let audit = ledger.verify_partition("tenant-1").unwrap();
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[0].record()["recordType"], "authorization");
    assert_eq!(audit[1].record()["recordType"], "outcome");

    backend.allow.store(false, Ordering::SeqCst);
    let revoked = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/capability-invocations/workforce.absence.propose",
            Some(
                serde_json::to_value(invocation_request(
                    &grant_set,
                    "inv-profile-revoked",
                    "effect-key-revoked",
                ))
                .unwrap(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::CONFLICT);
    assert_eq!(diagnostic_code(revoked).await, "KF-AUTH-003");

    backend.allow.store(true, Ordering::SeqCst);
    *backend.current.lock().unwrap() = revisions("r2");
    let stale = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/capability-invocations/workforce.absence.propose",
            Some(
                serde_json::to_value(invocation_request(
                    &grant_set,
                    "inv-profile-stale",
                    "effect-key-stale",
                ))
                .unwrap(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(diagnostic_code(stale).await, "KF-AUTH-004");

    drop(app);
    drop(store);
    let reopened = Arc::new(
        SqliteInvocationStore::open_with_clock(&database, clock)
            .await
            .unwrap(),
    );
    let status_app = provider_router(ProviderHttpState::new(services, reopened), http_verifier);
    let status_response = status_app
        .oneshot(request(
            Method::GET,
            "/v1/capability-invocations/inv-profile-1",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(status_response.status(), StatusCode::OK);
    let status: InvocationStatus = serde_json::from_slice(&body(status_response).await).unwrap();
    assert!(matches!(
        status,
        InvocationStatus::Succeeded { result, .. } if result == json!({"changed": true})
    ));
}

struct ProfileServices {
    catalog: CapabilityCatalog,
}

#[async_trait]
impl ProviderHttpServices for ProfileServices {
    async fn catalog(
        &self,
        _context: &ProviderRequestContext,
    ) -> Result<CapabilityCatalog, ProviderHttpError> {
        Ok(self.catalog.clone())
    }
}

struct ProfileHttpVerifier {
    admission: Arc<Mutex<AdmissionId>>,
}

#[async_trait]
impl ProviderPrincipalVerifier for ProfileHttpVerifier {
    async fn verify_human(
        &self,
        _headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedHumanAuthentication, Diagnostic> {
        Ok(VerifiedHumanAuthentication::new(
            human(),
            [header::AUTHORIZATION],
        ))
    }

    async fn verify_workload(
        &self,
        _headers: &axum::http::HeaderMap,
    ) -> Result<VerifiedWorkloadAuthentication, Diagnostic> {
        Ok(VerifiedWorkloadAuthentication::new(
            workload(self.admission.lock().unwrap().clone()),
            ["x-workload-token".parse().unwrap()],
        ))
    }
}

struct ProfileCoreVerifier {
    admission: Arc<Mutex<AdmissionId>>,
}

#[async_trait]
impl CorePrincipalVerifier for ProfileCoreVerifier {
    async fn verify(&self) -> Result<VerifiedProviderPrincipals, Diagnostic> {
        Ok(VerifiedProviderPrincipals::new(
            human(),
            workload(self.admission.lock().unwrap().clone()),
        ))
    }
}

struct ProfileAuthorization {
    current: Mutex<AuthorityRevisionSet>,
    allow: AtomicBool,
}

impl ProfileAuthorization {
    fn new(revisions: AuthorityRevisionSet) -> Self {
        Self {
            current: Mutex::new(revisions),
            allow: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl AuthorizationBackend for ProfileAuthorization {
    async fn list_admissible(
        &self,
        request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, Diagnostic> {
        if request.loaded_authority_revisions() != &*self.current.lock().unwrap() {
            return Err(policy_stale());
        }
        Ok(AdmissionAuthorizationResult::new(
            self.allow
                .load(Ordering::SeqCst)
                .then(|| request.capability().clone())
                .into_iter()
                .collect(),
        ))
    }

    async fn check(
        &self,
        request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, Diagnostic> {
        let revisions = self.current.lock().unwrap().clone();
        if !self.allow.load(Ordering::SeqCst) {
            return AuthorizationDecision::deny(
                "profile-revoked",
                SafeDenialReason::ResourceDenied,
            )
            .map_err(|_| policy_stale());
        }
        AuthorizationDecision::allow(
            "profile-allow",
            revisions,
            Timestamp::new(100),
            NarrowedAuthorizationConditions::new(
                vec![request.selected_resource().clone()],
                Timestamp::new(900),
                Vec::<PreconditionDescriptor>::new(),
            )
            .unwrap(),
        )
        .map_err(|_| policy_stale())
    }

    async fn revisions(&self) -> Result<AuthorityRevisionSet, Diagnostic> {
        Ok(self.current.lock().unwrap().clone())
    }
}

struct ProfileOperation;

#[async_trait]
impl CapabilityOperation for ProfileOperation {
    fn identity(&self) -> &CapabilityIdentity {
        static IDENTITY: std::sync::OnceLock<CapabilityIdentity> = std::sync::OnceLock::new();
        IDENTITY.get_or_init(identity)
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
        Ok(json!({"changed": true, "privateToken": "must-not-reach-status"}))
    }
}

struct UnusedEvidence;

#[async_trait]
impl InvocationEvidenceProvider for UnusedEvidence {
    async fn resolve(
        &self,
        _reference: &kiteframe_contract::ProtectedEvidenceRequestRef,
    ) -> Result<VerifiedEvidence, Diagnostic> {
        unreachable!("profile capability has no evidence requirement")
    }
}

struct UnusedCheckpointIssuer;

impl InvocationCheckpointIssuer for UnusedCheckpointIssuer {
    fn issue(
        &self,
        _proposal: &kiteframe_contract::EffectProposal,
    ) -> Result<kiteframe_contract::CheckpointRef, Diagnostic> {
        unreachable!("profile capability does not suspend")
    }
}

struct FixedClock(Timestamp);

impl InvocationClock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

impl InvocationStoreClock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

fn catalog() -> CapabilityCatalog {
    CapabilityCatalog::try_new(
        CatalogIdentity {
            name: "profile.catalog".to_owned(),
            revision: "1".to_owned(),
        },
        Timestamp::new(1),
        Some(Timestamp::new(600)),
        vec![descriptor()],
    )
    .unwrap()
}

fn descriptor() -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: identity(),
        summary: "Propose an employee absence".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["employeeId"],
            "properties": {"employeeId": {"type": "string"}}
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["changed", "privateToken"],
            "properties": {
                "changed": {"type": "boolean"},
                "privateToken": {"type": "string"}
            }
        }),
        stable_errors: vec![],
        execution_modes: modes(&[ExecutionMode::Deferred]),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({
            "type": "string",
            "pattern": "^tenant:[A-Za-z0-9-]+/employee:[A-Za-z0-9-]+$"
        }))
        .unwrap(),
        effect: EffectClassification::ReversibleWrite,
        idempotency: IdempotencyRequirement::Required {
            scope: IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: NonZeroU64::new(3_600).unwrap(),
        },
        freshness: FreshnessRequirement {
            max_admission_age_seconds: None,
            policy_revision_required: true,
            max_input_age_seconds: None,
        },
        preconditions: vec![],
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn locked_capability(descriptor: CapabilityDescriptor) -> LockedCapability {
    let [input, output, stable, safety] = descriptor_part_digests(&descriptor);
    LockedCapability::try_new(
        descriptor.identity().clone(),
        descriptor.clone(),
        *descriptor.descriptor_digest(),
        input,
        output,
        stable,
        safety,
    )
    .unwrap()
}

fn authority_sources(locked: &LockedCapability) -> Vec<AuthoritySource> {
    let grant = EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: locked.identity().clone(),
        resources: vec![selector(RESOURCE)],
        execution_modes: modes(&[ExecutionMode::Deferred]),
        maximum_effect: EffectClassification::ReversibleWrite,
        expires_at: Timestamp::new(500),
        required_evidence: RequiredEvidence::new(
            ConfirmationRequirement::None,
            ApprovalRequirement::None,
            ConsentRequirement::None,
        ),
        freshness: locked.descriptor().freshness().clone(),
        preconditions: vec![],
    })
    .unwrap();
    vec![
        AuthoritySource::try_new(
            "policy",
            "r1",
            AuthorityDomain::ALL
                .into_iter()
                .map(|domain| {
                    AuthorityPlane::new(domain, vec![AuthorityTerm::allow(grant.clone())])
                })
                .collect(),
        )
        .unwrap(),
    ]
}

fn admission_request(catalog: &CapabilityCatalog, locked: LockedCapability) -> AdmissionRequest {
    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("employee-7").unwrap(),
        agent: AgentRef::new("workforce-agent-2").unwrap(),
        task: TaskRef::new("absence-task-4").unwrap(),
        session: SessionRef::new("absence-session-3").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        catalog_identity: catalog.identity().clone(),
        catalog_digest: *catalog.catalog_digest(),
        required_capabilities: vec![
            RequestedCapability::try_new(identity(), vec![selector(RESOURCE)]).unwrap(),
        ],
        optional_capabilities: vec![],
        resolved_requirements: vec![
            ResolvedCapabilityRequirement::try_new(locked, true, vec![RESOURCE.to_owned()])
                .unwrap(),
        ],
        delegation_ancestry: DelegationAncestry::try_new(vec![]).unwrap(),
        contextual_facts: BTreeMap::new(),
        trace_context: trace_context(),
    })
    .unwrap()
}

fn invocation_request(
    grant_set: &kiteframe_contract::CapabilityGrantSet,
    invocation_id: &str,
    idempotency_key: &str,
) -> InvocationRequest {
    InvocationRequest::try_new(
        InvocationId::new(invocation_id).unwrap(),
        grant_set.admission_id().clone(),
        *grant_set.grant_digest(),
        *grant_set.delegation_ancestry_digest(),
        identity(),
        RESOURCE,
        json!({"employeeId": "employee-7"}),
        BTreeMap::new(),
        Some(idempotency_key.to_owned()),
        EvidenceReferences::default(),
        trace_context(),
    )
    .unwrap()
}

fn human() -> VerifiedHumanPrincipal {
    VerifiedHumanPrincipal::try_new(
        "tenant-1",
        "employee-7",
        ActorRef::new("employee-7").unwrap(),
        Timestamp::new(900),
    )
    .unwrap()
}

fn workload(admission: AdmissionId) -> VerifiedWorkloadPrincipal {
    VerifiedWorkloadPrincipal::try_new(
        "tenant-1",
        "workforce-harness-2",
        "run-9",
        AgentRef::new("workforce-agent-2").unwrap(),
        TaskRef::new("absence-task-4").unwrap(),
        SessionRef::new("absence-session-3").unwrap(),
        admission,
        Timestamp::new(900),
    )
    .unwrap()
}

#[allow(dead_code)]
fn correlated(admission: AdmissionId) -> AuthenticatedInvocationContext {
    correlate_principals(
        human(),
        workload(admission.clone()),
        PortableInvocationRefs::new(
            ActorRef::new("employee-7").unwrap(),
            AgentRef::new("workforce-agent-2").unwrap(),
            kiteframe_provider::RunRef::new("run-9").unwrap(),
            TaskRef::new("absence-task-4").unwrap(),
            SessionRef::new("absence-session-3").unwrap(),
            admission,
            Timestamp::new(100),
        ),
    )
    .unwrap()
}

fn identity() -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("workforce.absence.propose").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn selector(value: &str) -> NormalizedResourceSelector {
    NormalizedResourceSelector::new(value).unwrap()
}

fn modes(values: &[ExecutionMode]) -> NonEmptySet<ExecutionMode> {
    NonEmptySet::try_new(values.iter().copied().collect::<BTreeSet<_>>()).unwrap()
}

fn revisions(value: &str) -> AuthorityRevisionSet {
    AuthorityRevisionSet::try_new(vec![AuthorityRevision::try_new("policy", value).unwrap()])
        .unwrap()
}

fn trace_context() -> TraceContext {
    TraceContext::try_new(TRACEPARENT, None, BTreeMap::new()).unwrap()
}

fn request(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("traceparent", TRACEPARENT)
        .header(header::AUTHORIZATION, "Bearer profile-human")
        .header("x-workload-token", "profile-workload")
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            body.map(|value| Body::from(serde_json::to_vec(&value).unwrap()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap()
}

async fn body(response: axum::http::Response<Body>) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn diagnostic_code(response: axum::http::Response<Body>) -> String {
    let value: Value = serde_json::from_slice(&body(response).await).unwrap();
    value["diagnostics"][0]["code"].as_str().unwrap().to_owned()
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

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}

fn policy_stale() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PolicyStale,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        "profile authorization revision is stale",
    )
}
