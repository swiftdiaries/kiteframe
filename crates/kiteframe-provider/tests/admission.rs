use std::collections::BTreeSet;

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    AuthorityRevision, AuthorityRevisionSet, CapabilityCatalog, CapabilityDescriptor,
    CapabilityDescriptorParts, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CatalogIdentity,
    ConfirmationRequirement, ConsentRequirement, DelegationAncestry, EffectClassification,
    EffectiveCapabilityGrant, EffectiveCapabilityGrantParts, EvidenceRequirement, ExecutionMode,
    FreshnessRequirement, IdempotencyRequirement, LockedCapability, NonEmptySet,
    NormalizedResourceSelector, PolicyRevision, RequestedCapability, RequiredEvidence,
    ResolvedCapabilityRequirement, ResourceSelectorSchema, SessionRef, Sha256Digest, TaskRef,
    Timestamp, TraceContext,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AdmissionService,
    AdmissionServiceConfig, AuthenticatedInvocationContext, AuthorityDomain, AuthorityPlane,
    AuthoritySource, AuthorityTerm, AuthorizationBackend, AuthorizationDecision,
    InvocationAuthorizationRequest, PortableInvocationRefs, RunRef, VerifiedHumanPrincipal,
    VerifiedWorkloadPrincipal, correlate_principals,
};
use serde_json::json;

#[tokio::test]
async fn admission_proves_catalog_and_all_required_capabilities() {
    let service = service();
    let request = admission_request(authoritative_catalog().identity().clone(), None);
    let result = admit(&service, request).await.unwrap();

    assert_eq!(
        result.catalog_identity(),
        authoritative_catalog().identity()
    );
    assert_eq!(
        result.catalog_digest(),
        authoritative_catalog().catalog_digest()
    );
    assert_eq!(result.grants().len(), 2);
    assert_eq!(result.optional_denials().len(), 1);
    assert_eq!(
        result.optional_denials()[0].diagnostic().code.as_str(),
        "KF-AUTH-001"
    );
    assert_eq!(
        result.authority_revisions().entries(),
        [
            revision("deployment-policy", "deploy-7"),
            revision("openfga-model", "model-3"),
            revision("tenant-policy", "tenant-42"),
        ]
    );
    assert_eq!(
        result.authority_revisions().authority_revision_digest(),
        service.authority_revisions().authority_revision_digest()
    );

    let persisted = service
        .load_admission(result.admission_id(), result.grant_digest())
        .await
        .unwrap();
    assert_eq!(
        persisted.locked_capability(&identity("cases.read")),
        authoritative_registry()
            .iter()
            .find(|locked| locked.identity() == &identity("cases.read"))
    );
    assert_eq!(
        persisted.locked_capabilities().len(),
        3,
        "every provider-validated request lock is persisted, including optional denials"
    );
}

#[tokio::test]
async fn client_lock_drift_from_provider_registry_fails_closed() {
    let request = admission_request(
        authoritative_catalog().identity().clone(),
        Some(tampered_lock()),
    );
    let error = admit(&service(), request).await.unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-001");
}

#[tokio::test]
async fn required_capability_cannot_bypass_dynamic_admission_authorization() {
    let error = service()
        .admit(
            admission_request(authoritative_catalog().identity().clone(), None),
            authenticated_context(),
            &DenyAuthorizationBackend,
        )
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-001");
}

#[tokio::test]
async fn catalog_identity_or_digest_drift_fails_before_authority_evaluation() {
    let wrong_identity = CatalogIdentity {
        name: "different-catalog".to_owned(),
        revision: "catalog-7".to_owned(),
    };
    let identity_error = admit(&service(), admission_request(wrong_identity, None))
        .await
        .unwrap_err();
    assert_eq!(identity_error.code.as_str(), "KF-CAT-001");

    let digest_error = admit(
        &service(),
        admission_request_with_catalog_digest(digest(99)),
    )
        .await
        .unwrap_err();
    assert_eq!(digest_error.code.as_str(), "KF-CAT-001");
}

#[tokio::test]
async fn missing_required_authority_fails_the_whole_admission() {
    let mut sources = authority_sources();
    sources[0] = authority_source(
        "tenant-policy",
        "tenant-42",
        &[AuthorityDomain::Package, AuthorityDomain::Workload],
        allow_terms_for(&["cases.read"]),
    );
    let service = AdmissionService::try_new(
        authoritative_catalog(),
        authoritative_registry(),
        sources,
        service_config(),
    )
    .unwrap();

    let error = admit(
        &service,
        admission_request(
            authoritative_catalog().identity().clone(),
            None,
        ),
    )
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-001");
}

#[tokio::test]
async fn optional_authority_miss_is_safe_stable_and_creates_no_grant() {
    let result = admit(
        &service(),
        admission_request(
            authoritative_catalog().identity().clone(),
            None,
        ),
    )
        .await
        .unwrap();
    let denied = identity("cases.delete");

    assert!(
        result
            .grants()
            .iter()
            .all(|grant| grant.capability() != &denied)
    );
    let diagnostic = result.optional_denials()[0].diagnostic();
    assert_eq!(diagnostic.code.as_str(), "KF-AUTH-001");
    assert_eq!(
        diagnostic.message.as_str(),
        "optional capability was not admitted"
    );
    assert!(diagnostic.details.is_empty());
}

#[tokio::test]
async fn revision_source_order_does_not_change_the_canonical_grant_digest() {
    let request = admission_request(authoritative_catalog().identity().clone(), None);
    let first = admit(&service(), request.clone()).await.unwrap();
    let mut reversed = authority_sources();
    reversed.reverse();
    let second_service = AdmissionService::try_new(
        authoritative_catalog(),
        authoritative_registry(),
        reversed,
        service_config(),
    )
    .unwrap();
    let second = admit(&second_service, request).await.unwrap();

    assert_eq!(first.authority_revisions(), second.authority_revisions());
    assert_eq!(first.grant_digest(), second.grant_digest());
}

#[tokio::test]
async fn optional_unresolved_selector_becomes_a_stable_denial() {
    let mut sources = authority_sources_with_optional();
    sources[0] = authority_source(
        "tenant-policy",
        "tenant-42",
        &[AuthorityDomain::Package, AuthorityDomain::Workload],
        vec![
            allow_term("cases.read"),
            allow_term("cases.write"),
            AuthorityTerm::allow(grant_for(
                "cases.delete",
                "tenant:${context.tenant}/case:case-9",
                RequiredEvidence::new(
                    ConfirmationRequirement::None,
                    ApprovalRequirement::None,
                    ConsentRequirement::None,
                ),
            )),
        ],
    );
    let service = AdmissionService::try_new(
        authoritative_catalog(),
        authoritative_registry(),
        sources,
        service_config(),
    )
    .unwrap();

    let result = admit(
        &service,
        admission_request(
            authoritative_catalog().identity().clone(),
            None,
        ),
    )
        .await
        .unwrap();

    assert_optional_denial(&result);
}

#[tokio::test]
async fn optional_conflicting_evidence_becomes_a_stable_denial() {
    let mut sources = authority_sources_with_optional();
    sources[0] = authority_source(
        "tenant-policy",
        "tenant-42",
        &[AuthorityDomain::Package, AuthorityDomain::Workload],
        vec![
            allow_term("cases.read"),
            allow_term("cases.write"),
            AuthorityTerm::allow(grant_for(
                "cases.delete",
                "tenant:t1/case:case-9",
                confirmation("tenant_confirmation"),
            )),
        ],
    );
    sources[1] = authority_source(
        "deployment-policy",
        "deploy-7",
        &[AuthorityDomain::Deployment],
        vec![
            allow_term("cases.read"),
            allow_term("cases.write"),
            AuthorityTerm::allow(grant_for(
                "cases.delete",
                "tenant:t1/case:case-9",
                confirmation("deployment_confirmation"),
            )),
        ],
    );
    let service = AdmissionService::try_new(
        authoritative_catalog(),
        authoritative_registry(),
        sources,
        service_config(),
    )
    .unwrap();

    let result = admit(
        &service,
        admission_request(
            authoritative_catalog().identity().clone(),
            None,
        ),
    )
        .await
        .unwrap();

    assert_optional_denial(&result);
}

#[tokio::test]
async fn omitting_any_mandatory_authority_plane_denies_admission() {
    for omitted in AuthorityDomain::ALL {
        let service = AdmissionService::try_new(
            authoritative_catalog(),
            authoritative_registry(),
            authority_sources_without(omitted),
            service_config(),
        )
        .unwrap();

        let error = admit(
            &service,
            admission_request(
                authoritative_catalog().identity().clone(),
                None,
            ),
        )
            .await
            .unwrap_err();

        assert_eq!(
            error.code.as_str(),
            "KF-AUTH-001",
            "missing {omitted:?} must default deny"
        );
    }
}

#[tokio::test]
async fn session_expiry_caps_each_capability_grant() {
    let service = AdmissionService::try_new(
        authoritative_catalog(),
        authoritative_registry(),
        authority_sources(),
        AdmissionServiceConfig {
            expires_at: Timestamp::new(5_000),
            ..service_config()
        },
    )
    .unwrap();

    let result = admit(
        &service,
        admission_request(
            authoritative_catalog().identity().clone(),
            None,
        ),
    )
        .await
        .unwrap();

    assert!(
        result
            .grants()
            .iter()
            .all(|grant| grant.expires_at() == Timestamp::new(5_000))
    );
}

#[test]
fn admission_expiry_cannot_outlive_the_authoritative_catalog() {
    let error = match AdmissionService::try_new(
        authoritative_catalog(),
        authoritative_registry(),
        authority_sources(),
        AdmissionServiceConfig {
            expires_at: Timestamp::new(10_001),
            ..service_config()
        },
    ) {
        Ok(_) => panic!("admission service must not outlive its catalog"),
        Err(error) => error,
    };

    assert_eq!(error[0].code.as_str(), "KF-CAT-001");
}

#[tokio::test]
async fn every_resolved_requirement_must_map_to_exactly_one_request_entry() {
    let error = admit(&service(), admission_request_without_optional_entry())
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-001");
}

fn service() -> AdmissionService {
    AdmissionService::try_new(
        authoritative_catalog(),
        authoritative_registry(),
        authority_sources(),
        service_config(),
    )
    .unwrap()
}

fn service_config() -> AdmissionServiceConfig {
    AdmissionServiceConfig {
        issued_at: Timestamp::new(1_000),
        expires_at: Timestamp::new(8_000),
        policy_revision: PolicyRevision::new("policy-9").unwrap(),
    }
}

async fn admit(
    service: &AdmissionService,
    request: AdmissionRequest,
) -> Result<kiteframe_contract::CapabilityGrantSet, kiteframe_contract::Diagnostic> {
    service
        .admit(request, authenticated_context(), &TestAuthorizationBackend)
        .await
}

struct TestAuthorizationBackend;
struct DenyAuthorizationBackend;

#[async_trait]
impl AuthorizationBackend for TestAuthorizationBackend {
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
        unreachable!("admission tests do not perform invocation checks")
    }

    async fn revisions(
        &self,
    ) -> Result<AuthorityRevisionSet, kiteframe_contract::Diagnostic> {
        AuthorityRevisionSet::try_new(vec![
            revision("deployment-policy", "deploy-7"),
            revision("openfga-model", "model-3"),
            revision("tenant-policy", "tenant-42"),
        ])
        .map_err(|message| {
            kiteframe_contract::Diagnostic::error(
                kiteframe_contract::DiagnosticCode::PolicyStale,
                kiteframe_contract::DiagnosticCategory::Authorization,
                kiteframe_contract::DiagnosticStage::Admit,
                message,
            )
        })
    }
}

#[async_trait]
impl AuthorizationBackend for DenyAuthorizationBackend {
    async fn list_admissible(
        &self,
        _request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, kiteframe_contract::Diagnostic> {
        Ok(AdmissionAuthorizationResult::new(vec![]))
    }

    async fn check(
        &self,
        _request: &InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, kiteframe_contract::Diagnostic> {
        unreachable!("admission tests do not perform invocation checks")
    }

    async fn revisions(
        &self,
    ) -> Result<AuthorityRevisionSet, kiteframe_contract::Diagnostic> {
        TestAuthorizationBackend.revisions().await
    }
}

fn authenticated_context() -> AuthenticatedInvocationContext {
    correlate_principals(
        VerifiedHumanPrincipal::try_new(
            "tenant-1",
            "human-7",
            ActorRef::new("actor-7").unwrap(),
            Timestamp::new(9_000),
        )
        .unwrap(),
        VerifiedWorkloadPrincipal::try_new(
            "tenant-1",
            "workload-2",
            "run-9",
            AgentRef::new("agent-2").unwrap(),
            TaskRef::new("task-4").unwrap(),
            SessionRef::new("session-3").unwrap(),
            AdmissionId::new("prior-admission").unwrap(),
            Timestamp::new(9_000),
        )
        .unwrap(),
        PortableInvocationRefs::new(
            ActorRef::new("actor-7").unwrap(),
            AgentRef::new("agent-2").unwrap(),
            RunRef::new("run-9").unwrap(),
            TaskRef::new("task-4").unwrap(),
            SessionRef::new("session-3").unwrap(),
            AdmissionId::new("prior-admission").unwrap(),
            Timestamp::new(1_000),
        ),
    )
    .unwrap()
}

fn authority_sources() -> Vec<AuthoritySource> {
    authority_sources_without_optional_omission(None)
}

fn authority_sources_without(omitted: AuthorityDomain) -> Vec<AuthoritySource> {
    authority_sources_without_optional_omission(Some(omitted))
}

fn authority_sources_without_optional_omission(
    omitted: Option<AuthorityDomain>,
) -> Vec<AuthoritySource> {
    let domains = |values: &[AuthorityDomain]| {
        values
            .iter()
            .copied()
            .filter(|domain| Some(*domain) != omitted)
            .collect::<Vec<_>>()
    };
    vec![
        authority_source(
            "tenant-policy",
            "tenant-42",
            &domains(&[AuthorityDomain::Package, AuthorityDomain::Workload]),
            allow_terms_for(&["cases.read", "cases.write"]),
        ),
        authority_source(
            "deployment-policy",
            "deploy-7",
            &domains(&[AuthorityDomain::Deployment]),
            allow_terms_for(&["cases.read", "cases.write"]),
        ),
        authority_source(
            "openfga-model",
            "model-3",
            &domains(&[
                AuthorityDomain::Human,
                AuthorityDomain::Task,
                AuthorityDomain::Session,
            ]),
            allow_terms_for(&["cases.read", "cases.write"]),
        ),
    ]
}

fn authority_sources_with_optional() -> Vec<AuthoritySource> {
    vec![
        authority_source(
            "tenant-policy",
            "tenant-42",
            &[AuthorityDomain::Package, AuthorityDomain::Workload],
            allow_terms_for(&["cases.read", "cases.write", "cases.delete"]),
        ),
        authority_source(
            "deployment-policy",
            "deploy-7",
            &[AuthorityDomain::Deployment],
            allow_terms_for(&["cases.read", "cases.write", "cases.delete"]),
        ),
        authority_source(
            "openfga-model",
            "model-3",
            &[
                AuthorityDomain::Human,
                AuthorityDomain::Task,
                AuthorityDomain::Session,
            ],
            allow_terms_for(&["cases.read", "cases.write", "cases.delete"]),
        ),
    ]
}

fn authority_source(
    source: &str,
    revision: &str,
    domains: &[AuthorityDomain],
    terms: Vec<AuthorityTerm>,
) -> AuthoritySource {
    AuthoritySource::try_new(
        source,
        revision,
        domains
            .iter()
            .map(|domain| AuthorityPlane::new(*domain, terms.clone()))
            .collect(),
    )
    .unwrap()
}

fn allow_terms_for(names: &[&str]) -> Vec<AuthorityTerm> {
    names.iter().map(|name| allow_term(name)).collect()
}

fn allow_term(name: &str) -> AuthorityTerm {
    AuthorityTerm::allow(grant(
        name,
        if name == "cases.read" {
            "tenant:t1/case:case-7"
        } else {
            "tenant:t1/case:case-9"
        },
    ))
}

fn admission_request(
    catalog_identity: CatalogIdentity,
    replacement_lock: Option<LockedCapability>,
) -> AdmissionRequest {
    let catalog_digest = *authoritative_catalog().catalog_digest();
    admission_request_with(catalog_identity, catalog_digest, replacement_lock)
}

fn admission_request_with_catalog_digest(catalog_digest: Sha256Digest) -> AdmissionRequest {
    admission_request_with(
        authoritative_catalog().identity().clone(),
        catalog_digest,
        None,
    )
}

fn admission_request_with(
    catalog_identity: CatalogIdentity,
    catalog_digest: Sha256Digest,
    replacement_lock: Option<LockedCapability>,
) -> AdmissionRequest {
    admission_request_with_options(catalog_identity, catalog_digest, replacement_lock, true)
}

fn admission_request_without_optional_entry() -> AdmissionRequest {
    admission_request_with_options(
        authoritative_catalog().identity().clone(),
        *authoritative_catalog().catalog_digest(),
        None,
        false,
    )
}

fn admission_request_with_options(
    catalog_identity: CatalogIdentity,
    catalog_digest: Sha256Digest,
    replacement_lock: Option<LockedCapability>,
    include_optional_entry: bool,
) -> AdmissionRequest {
    let mut locks = authoritative_registry();
    if let Some(replacement) = replacement_lock {
        let position = locks
            .iter()
            .position(|lock| lock.identity() == replacement.identity())
            .unwrap();
        locks[position] = replacement;
    }
    let resolved_requirements = locks
        .into_iter()
        .map(|lock| {
            let name = lock.identity().name().as_str().to_owned();
            let resource = if name == "cases.read" {
                "tenant:t1/case:*"
            } else {
                "tenant:t1/case:case-9"
            };
            ResolvedCapabilityRequirement::try_new(
                lock,
                name != "cases.delete",
                vec![resource.to_owned()],
            )
            .unwrap()
        })
        .collect();

    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor-1").unwrap(),
        agent: AgentRef::new("agent-1").unwrap(),
        task: TaskRef::new("task-1").unwrap(),
        session: SessionRef::new("session-1").unwrap(),
        portable_digest: digest(11),
        lock_digest: digest(12),
        resolved_digest: digest(13),
        catalog_identity,
        catalog_digest,
        required_capabilities: vec![
            requested("cases.read", "tenant:t1/case:case-7"),
            requested("cases.write", "tenant:t1/case:case-9"),
        ],
        optional_capabilities: include_optional_entry
            .then(|| requested("cases.delete", "tenant:t1/case:case-9"))
            .into_iter()
            .collect(),
        resolved_requirements,
        delegation_ancestry: DelegationAncestry::try_new(vec![]).unwrap(),
        contextual_facts: Default::default(),
        trace_context: TraceContext::try_new(
            "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
            None,
            Default::default(),
        )
        .unwrap(),
    })
    .unwrap()
}

fn authoritative_catalog() -> CapabilityCatalog {
    CapabilityCatalog::try_new(
        CatalogIdentity {
            name: "test-catalog".to_owned(),
            revision: "catalog-7".to_owned(),
        },
        Timestamp::new(100),
        Some(Timestamp::new(10_000)),
        ["cases.read", "cases.write", "cases.delete"]
            .into_iter()
            .map(descriptor)
            .collect(),
    )
    .unwrap()
}

fn authoritative_registry() -> Vec<LockedCapability> {
    ["cases.read", "cases.write", "cases.delete"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| locked(descriptor(name), index as u8 + 1))
        .collect()
}

fn tampered_lock() -> LockedCapability {
    let mut descriptor = descriptor("cases.read");
    let serialized = serde_json::to_value(&descriptor).unwrap();
    let mut parts = descriptor_parts("cases.read");
    parts.summary = "Tampered client descriptor".to_owned();
    descriptor = CapabilityDescriptor::try_new(parts).unwrap();
    assert_ne!(
        serialized,
        serde_json::to_value(&descriptor).unwrap(),
        "fixture must alter the embedded locked descriptor"
    );
    locked(descriptor, 1)
}

fn locked(descriptor: CapabilityDescriptor, salt: u8) -> LockedCapability {
    LockedCapability::try_new(
        descriptor.identity().clone(),
        descriptor.clone(),
        *descriptor.descriptor_digest(),
        digest(salt + 20),
        digest(salt + 30),
        digest(salt + 40),
        digest(salt + 50),
    )
    .unwrap()
}

fn descriptor(name: &str) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(descriptor_parts(name)).unwrap()
}

fn descriptor_parts(name: &str) -> CapabilityDescriptorParts {
    CapabilityDescriptorParts {
        identity: identity(name),
        summary: format!("Capability {name}"),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
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
    }
}

fn requested(name: &str, resource: &str) -> RequestedCapability {
    RequestedCapability::try_new(identity(name), vec![selector(resource)]).unwrap()
}

fn grant(name: &str, resource: &str) -> EffectiveCapabilityGrant {
    grant_for(
        name,
        resource,
        RequiredEvidence::new(
            ConfirmationRequirement::None,
            ApprovalRequirement::None,
            ConsentRequirement::None,
        ),
    )
}

fn grant_for(
    name: &str,
    resource: &str,
    required_evidence: RequiredEvidence,
) -> EffectiveCapabilityGrant {
    EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: identity(name),
        resources: vec![selector(resource)],
        execution_modes: modes(&[ExecutionMode::Immediate]),
        maximum_effect: EffectClassification::ReadOnly,
        expires_at: Timestamp::new(7_200),
        required_evidence,
        freshness: FreshnessRequirement::default(),
        preconditions: vec![],
    })
    .unwrap()
}

fn confirmation(kind: &str) -> RequiredEvidence {
    RequiredEvidence::new(
        ConfirmationRequirement::Required {
            evidence: EvidenceRequirement {
                kind: kind.to_owned(),
                issuer: None,
            },
        },
        ApprovalRequirement::None,
        ConsentRequirement::None,
    )
}

fn assert_optional_denial(result: &kiteframe_contract::CapabilityGrantSet) {
    assert_eq!(result.grants().len(), 2);
    assert_eq!(result.optional_denials().len(), 1);
    let denial = &result.optional_denials()[0];
    assert_eq!(denial.capability(), &identity("cases.delete"));
    assert_eq!(denial.diagnostic().code.as_str(), "KF-AUTH-001");
    assert_eq!(
        denial.diagnostic().message.as_str(),
        "optional capability was not admitted"
    );
    assert!(denial.diagnostic().details.is_empty());
}

fn identity(name: &str) -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new(name).unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn modes(values: &[ExecutionMode]) -> NonEmptySet<ExecutionMode> {
    NonEmptySet::try_new(BTreeSet::from_iter(values.iter().copied())).unwrap()
}

fn selector(value: &str) -> NormalizedResourceSelector {
    NormalizedResourceSelector::new(value).unwrap()
}

fn revision(source: &str, value: &str) -> AuthorityRevision {
    AuthorityRevision::try_new(source, value).unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}
