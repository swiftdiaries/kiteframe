use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, ApprovalRequirement, AuthorityRevision, AuthorityRevisionSet,
    CapabilityDescriptor, CapabilityDescriptorParts, CapabilityIdentity, CapabilityName,
    CapabilityReleaseVersion, ConfirmationRequirement, ConsentRequirement, EffectClassification,
    EffectiveCapabilityGrant, EffectiveCapabilityGrantParts, ExecutionMode, FreshnessRequirement,
    IdempotencyRequirement, LockedCapability, NonEmptySet, NormalizedResourceSelector,
    RequiredEvidence, ResourceSelectorSchema, SessionRef, Sha256Digest, TaskRef, Timestamp,
    TraceContext,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AuthenticatedInvocationContext,
    AuthorizationBackend, AuthorizationDecision, CapabilityOperation,
    InvocationAuthorizationRequest, InvocationContext, NarrowedAuthorizationConditions,
    OperationFailure, OperationRegistry, PortableInvocationRefs, Precondition, SafeDenialReason,
    VerifiedHumanPrincipal, VerifiedWorkloadPrincipal, correlate_principals,
    require_current_authorization,
};
use serde_json::{Value, json};

#[tokio::test]
async fn duplicate_operation_registration_is_rejected() {
    let mut registry = OperationRegistry::new();
    registry.register(ReadOperation).unwrap();
    let error = registry.register(ReadOperation).unwrap_err();

    assert_eq!(error.code.as_str(), "KF-RUNTIME-001");
}

#[tokio::test]
async fn invocation_uses_current_check_not_admission_decision() {
    let backend = FakeAuthorizationBackend::default();
    let request = authorization_request();

    let admitted = backend
        .list_admissible(&AdmissionAuthorizationRequest::new(
            authenticated_context(),
            capability_identity("1.0.0"),
            selector("case:42"),
            revisions("admission-7"),
        ))
        .await
        .unwrap();
    assert_eq!(admitted.admissible().len(), 1);

    let error = require_current_authorization(&backend, &request)
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert_eq!(backend.invocation_checks.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn frozen_registry_uses_exact_version_and_validates_stable_projection() {
    let mut registry = OperationRegistry::new();
    registry.register(ReadOperation).unwrap();
    let registry = registry.freeze().unwrap();
    let context = invocation_context("1.0.0");

    let result = registry
        .execute(&context, &[], json!({"caseId": "42"}))
        .await
        .unwrap();
    assert_eq!(result, json!({"caseId": "42", "summary": "stable"}));

    let wrong_version = invocation_context("2.0.0");
    let error = registry
        .execute(&wrong_version, &[], json!({"caseId": "42"}))
        .await
        .unwrap_err();
    assert_eq!(error.diagnostic().code.as_str(), "KF-RUNTIME-001");
}

#[tokio::test]
async fn output_with_deployment_internal_fields_is_rejected_by_locked_schema() {
    let mut registry = OperationRegistry::new();
    registry.register(LeakyReadOperation).unwrap();
    let registry = registry.freeze().unwrap();

    let error = registry
        .execute(&invocation_context("1.0.0"), &[], json!({"caseId": "42"}))
        .await
        .unwrap_err();
    assert_eq!(error.diagnostic().code.as_str(), "KF-CAP-002");
}

#[derive(Default)]
struct FakeAuthorizationBackend {
    invocation_checks: AtomicUsize,
}

#[async_trait]
impl AuthorizationBackend for FakeAuthorizationBackend {
    async fn list_admissible(
        &self,
        request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, kiteframe_contract::Diagnostic> {
        Ok(AdmissionAuthorizationResult::new(vec![
            request.capability().clone(),
        ]))
    }

    async fn check(
        &self,
        _request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, kiteframe_contract::Diagnostic> {
        self.invocation_checks.fetch_add(1, Ordering::SeqCst);
        Ok(
            AuthorizationDecision::deny("decision-current-deny", SafeDenialReason::ResourceDenied)
                .unwrap(),
        )
    }

    async fn revisions(&self) -> Result<AuthorityRevisionSet, kiteframe_contract::Diagnostic> {
        Ok(revisions("current-8"))
    }
}

struct ReadOperation;

#[async_trait]
impl CapabilityOperation for ReadOperation {
    fn identity(&self) -> &CapabilityIdentity {
        static IDENTITY: std::sync::OnceLock<CapabilityIdentity> = std::sync::OnceLock::new();
        IDENTITY.get_or_init(|| capability_identity("1.0.0"))
    }

    async fn validate_preconditions(
        &self,
        _context: &InvocationContext,
        _preconditions: &[Precondition],
    ) -> Result<(), kiteframe_contract::Diagnostic> {
        Ok(())
    }

    async fn execute(
        &self,
        _context: &InvocationContext,
        arguments: Value,
    ) -> Result<Value, OperationFailure> {
        Ok(json!({
            "caseId": arguments["caseId"],
            "summary": "stable",
        }))
    }
}

struct LeakyReadOperation;

#[async_trait]
impl CapabilityOperation for LeakyReadOperation {
    fn identity(&self) -> &CapabilityIdentity {
        static IDENTITY: std::sync::OnceLock<CapabilityIdentity> = std::sync::OnceLock::new();
        IDENTITY.get_or_init(|| capability_identity("1.0.0"))
    }

    async fn validate_preconditions(
        &self,
        _context: &InvocationContext,
        _preconditions: &[Precondition],
    ) -> Result<(), kiteframe_contract::Diagnostic> {
        Ok(())
    }

    async fn execute(
        &self,
        _context: &InvocationContext,
        _arguments: Value,
    ) -> Result<Value, OperationFailure> {
        Ok(json!({
            "caseId": "42",
            "summary": "stable",
            "fieldDecision": "internal-policy-rule",
        }))
    }
}

fn authorization_request() -> InvocationAuthorizationRequest {
    InvocationAuthorizationRequest::new(
        authenticated_context(),
        capability_identity("1.0.0"),
        selector("case:42"),
        digest(7),
        revisions("admission-7"),
    )
}

fn invocation_context(version: &str) -> InvocationContext {
    let identity = capability_identity(version);
    let descriptor = descriptor(identity.clone());
    let locked = LockedCapability::try_new(
        identity.clone(),
        descriptor.clone(),
        *descriptor.descriptor_digest(),
        digest(1),
        digest(2),
        digest(3),
        digest(4),
    )
    .unwrap();
    let grant = EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: identity.clone(),
        resources: vec![selector("case:42")],
        execution_modes: modes(&[ExecutionMode::Immediate]),
        maximum_effect: EffectClassification::ReadOnly,
        expires_at: Timestamp::new(450),
        required_evidence: RequiredEvidence::new(
            ConfirmationRequirement::None,
            ApprovalRequirement::None,
            ConsentRequirement::None,
        ),
        freshness: FreshnessRequirement::default(),
        preconditions: vec![],
    })
    .unwrap();
    InvocationContext::try_new(
        authenticated_context(),
        identity,
        selector("case:42"),
        trace_context(),
        locked,
        grant,
        digest(7),
        revisions("current-8"),
        AuthorizationDecision::allow(
            "decision-current-allow",
            revisions("current-8"),
            Timestamp::new(200),
            NarrowedAuthorizationConditions::new(
                vec![selector("case:42")],
                Timestamp::new(400),
                vec![],
            )
            .unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn authenticated_context() -> AuthenticatedInvocationContext {
    correlate_principals(
        VerifiedHumanPrincipal::try_new(
            "tenant-1",
            "human-7",
            ActorRef::new("actor-7").unwrap(),
            Timestamp::new(500),
        )
        .unwrap(),
        VerifiedWorkloadPrincipal::try_new(
            "tenant-1",
            "harness-2",
            "run-9",
            AgentRef::new("agent-2").unwrap(),
            TaskRef::new("task-4").unwrap(),
            SessionRef::new("session-3").unwrap(),
            AdmissionId::new("admission-5").unwrap(),
            Timestamp::new(450),
        )
        .unwrap(),
        PortableInvocationRefs::new(
            ActorRef::new("actor-7").unwrap(),
            AgentRef::new("agent-2").unwrap(),
            TaskRef::new("task-4").unwrap(),
            SessionRef::new("session-3").unwrap(),
            AdmissionId::new("admission-5").unwrap(),
            Timestamp::new(100),
        ),
    )
    .unwrap()
}

fn descriptor(identity: CapabilityIdentity) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity,
        summary: "Read a stable case projection".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId"],
            "properties": {"caseId": {"type": "string"}},
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId", "summary"],
            "properties": {
                "caseId": {"type": "string"},
                "summary": {"type": "string"},
            },
        }),
        stable_errors: vec![],
        execution_modes: modes(&[ExecutionMode::Immediate]),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect: EffectClassification::ReadOnly,
        idempotency: IdempotencyRequirement::None,
        freshness: FreshnessRequirement::default(),
        preconditions: vec![],
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn capability_identity(version: &str) -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.read").unwrap(),
        CapabilityReleaseVersion::new(version).unwrap(),
    )
    .unwrap()
}

fn selector(value: &str) -> NormalizedResourceSelector {
    NormalizedResourceSelector::new(value).unwrap()
}

fn modes(values: &[ExecutionMode]) -> NonEmptySet<ExecutionMode> {
    NonEmptySet::try_new(values.iter().copied().collect::<BTreeSet<_>>()).unwrap()
}

fn revisions(revision: &str) -> AuthorityRevisionSet {
    AuthorityRevisionSet::try_new(vec![
        AuthorityRevision::try_new("policy", revision).unwrap(),
    ])
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
