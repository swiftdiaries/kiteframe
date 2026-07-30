use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;
use kiteframe_contract::{
    ApprovalRequirement, AuthorityRevisionSet, CapabilityGrantSet, CapabilityIdentity,
    ConfirmationRequirement, ConsentRequirement, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, EffectClassification, EffectProposal, EffectiveCapabilityGrant, EvidenceKind,
    EvidenceRequirement, ExecutionMode, InvocationOutcome, InvocationRequest, LockedCapability,
    NormalizedResourceSelector, ProtectedEvidenceRequestRef, RetryClass, Sha256Digest, Suspension,
    Timestamp, resource_selector_is_subset_of,
};
use sha2::{Digest, Sha256};

use crate::{
    AuthenticatedInvocationContext, AuthorizationDecision, InvocationAuthorizationRequest,
    InvocationContext, NarrowedAuthorizationConditions, OperationFailure, OperationRegistry,
    PortableInvocationRefs, Precondition, ProviderPrincipalVerifier, correlate_principals,
};

#[derive(Clone, Debug)]
pub struct InvocationAdmission {
    grant_set: CapabilityGrantSet,
    locked_capabilities: BTreeMap<CapabilityIdentity, LockedCapability>,
}

impl InvocationAdmission {
    pub fn try_new(
        grant_set: CapabilityGrantSet,
        locked_capabilities: Vec<LockedCapability>,
    ) -> Result<Self, Diagnostic> {
        let mut indexed = BTreeMap::new();
        for locked in locked_capabilities {
            if indexed.insert(locked.identity().clone(), locked).is_some() {
                return Err(capability_error(
                    "persisted admission contains a duplicate locked capability",
                ));
            }
        }
        for grant in grant_set.grants() {
            let Some(locked) = indexed.get(grant.capability()) else {
                return Err(capability_error(
                    "persisted grant has no exact provider-validated locked capability",
                ));
            };
            validate_grant_against_locked(grant, locked, grant_set.expires_at())?;
        }
        Ok(Self {
            grant_set,
            locked_capabilities: indexed,
        })
    }

    pub fn grant_set(&self) -> &CapabilityGrantSet {
        &self.grant_set
    }

    pub fn locked_capability(
        &self,
        identity: &CapabilityIdentity,
    ) -> Result<&LockedCapability, Diagnostic> {
        self.locked_capabilities
            .get(identity)
            .ok_or_else(|| capability_error("invocation capability is absent from the admission"))
    }

    pub fn effective_grant(
        &self,
        identity: &CapabilityIdentity,
    ) -> Result<&EffectiveCapabilityGrant, Diagnostic> {
        let mut matching = self
            .grant_set
            .grants()
            .iter()
            .filter(|grant| grant.capability() == identity);
        let grant = matching
            .next()
            .ok_or_else(|| grant_error("invocation capability was not granted"))?;
        if matching.next().is_some() {
            return Err(grant_error(
                "persisted admission contains duplicate exact capability grants",
            ));
        }
        Ok(grant)
    }
}

#[async_trait]
pub trait InvocationAdmissionStore: Send + Sync {
    async fn load(
        &self,
        admission_id: &kiteframe_contract::AdmissionId,
        grant_digest: &Sha256Digest,
    ) -> Result<InvocationAdmission, Diagnostic>;
}

pub struct InMemoryInvocationAdmissionStore {
    admissions: BTreeMap<(String, String), InvocationAdmission>,
}

impl InMemoryInvocationAdmissionStore {
    pub fn new(admissions: Vec<InvocationAdmission>) -> Result<Self, Diagnostic> {
        let mut indexed = BTreeMap::new();
        for admission in admissions {
            let key = (
                admission.grant_set().admission_id().as_str().to_owned(),
                admission.grant_set().grant_digest().to_string(),
            );
            if indexed.insert(key, admission).is_some() {
                return Err(capability_error(
                    "invocation admission store contains a duplicate exact admission",
                ));
            }
        }
        Ok(Self {
            admissions: indexed,
        })
    }
}

#[async_trait]
impl InvocationAdmissionStore for InMemoryInvocationAdmissionStore {
    async fn load(
        &self,
        admission_id: &kiteframe_contract::AdmissionId,
        grant_digest: &Sha256Digest,
    ) -> Result<InvocationAdmission, Diagnostic> {
        self.admissions
            .get(&(admission_id.as_str().to_owned(), grant_digest.to_string()))
            .cloned()
            .ok_or_else(|| capability_error("admission ID and canonical grant digest do not match"))
    }
}

pub trait InvocationClock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// Deployment-owned issuer for unguessable, globally unique checkpoint references.
///
/// Implementations must include at least 256 bits of cryptographically secure
/// randomness and may additionally namespace the reference by proposal.
pub trait InvocationCheckpointIssuer: Send + Sync {
    fn issue(
        &self,
        proposal: &EffectProposal,
    ) -> Result<kiteframe_contract::CheckpointRef, Diagnostic>;
}

pub trait InvocationEventSink: Send + Sync {
    fn record(&self, event: &'static str);
}

struct NoopEventSink;

impl InvocationEventSink for NoopEventSink {
    fn record(&self, _event: &'static str) {}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedEvidence {
    reference: ProtectedEvidenceRequestRef,
    evidence_kind: EvidenceKind,
    requirement_kind: String,
    principal_ref: String,
    issuer: Option<String>,
    capability: CapabilityIdentity,
    selected_resource: NormalizedResourceSelector,
    issued_at: Timestamp,
    expires_at: Timestamp,
    proposal_digest: Sha256Digest,
}

impl VerifiedEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        reference: ProtectedEvidenceRequestRef,
        evidence_kind: EvidenceKind,
        requirement_kind: impl Into<String>,
        principal_ref: impl Into<String>,
        issuer: Option<impl Into<String>>,
        capability: CapabilityIdentity,
        selected_resource: NormalizedResourceSelector,
        issued_at: Timestamp,
        expires_at: Timestamp,
        proposal_digest: Sha256Digest,
    ) -> Result<Self, String> {
        let requirement_kind = requirement_kind.into();
        let principal_ref = principal_ref.into();
        let issuer = issuer.map(Into::into);
        if requirement_kind.trim().is_empty() || principal_ref.trim().is_empty() {
            return Err("verified evidence kind and principal are required".to_owned());
        }
        if issuer.as_ref().is_some_and(|value| value.trim().is_empty()) {
            return Err("verified evidence issuer must not be empty".to_owned());
        }
        if expires_at <= issued_at {
            return Err("verified evidence expiry must be after issue time".to_owned());
        }
        Ok(Self {
            reference,
            evidence_kind,
            requirement_kind,
            principal_ref,
            issuer,
            capability,
            selected_resource,
            issued_at,
            expires_at,
            proposal_digest,
        })
    }

    pub fn reference(&self) -> &ProtectedEvidenceRequestRef {
        &self.reference
    }
}

#[async_trait]
pub trait InvocationEvidenceProvider: Send + Sync {
    async fn resolve(
        &self,
        reference: &ProtectedEvidenceRequestRef,
    ) -> Result<VerifiedEvidence, Diagnostic>;
}

#[derive(Clone, Debug)]
pub struct ResumeRequest {
    request: InvocationRequest,
    suspension: Suspension,
}

impl ResumeRequest {
    pub fn new(request: InvocationRequest, suspension: Suspension) -> Self {
        Self {
            request,
            suspension,
        }
    }
}

#[derive(Clone)]
struct PendingSuspension {
    suspension: Suspension,
    snapshot: AdmissionSnapshot,
}

#[derive(Clone)]
enum SuspensionState {
    Pending(Box<PendingSuspension>),
    InFlight,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AdmissionSnapshot {
    grant_digest: Sha256Digest,
    catalog_digest: Sha256Digest,
    authority_revisions: AuthorityRevisionSet,
    descriptor_digest: Sha256Digest,
    input_schema_digest: Sha256Digest,
    output_schema_digest: Sha256Digest,
    stable_error_set_digest: Sha256Digest,
    safety_metadata_digest: Sha256Digest,
    grant: EffectiveCapabilityGrant,
}

impl AdmissionSnapshot {
    fn new(
        admission: &InvocationAdmission,
        locked: &LockedCapability,
        grant: &EffectiveCapabilityGrant,
    ) -> Self {
        Self {
            grant_digest: *admission.grant_set().grant_digest(),
            catalog_digest: *admission.grant_set().catalog_digest(),
            authority_revisions: admission.grant_set().authority_revisions().clone(),
            descriptor_digest: *locked.descriptor_digest(),
            input_schema_digest: *locked.input_schema_digest(),
            output_schema_digest: *locked.output_schema_digest(),
            stable_error_set_digest: *locked.stable_error_set_digest(),
            safety_metadata_digest: *locked.safety_metadata_digest(),
            grant: grant.clone(),
        }
    }
}

pub struct InvocationService {
    admissions: Arc<dyn InvocationAdmissionStore>,
    principal_verifier: Arc<dyn ProviderPrincipalVerifier>,
    operations: OperationRegistry,
    evidence: Arc<dyn InvocationEvidenceProvider>,
    clock: Arc<dyn InvocationClock>,
    checkpoint_issuer: Arc<dyn InvocationCheckpointIssuer>,
    events: Arc<dyn InvocationEventSink>,
    pending: Mutex<BTreeMap<String, SuspensionState>>,
}

impl InvocationService {
    pub fn try_new(
        admissions: Arc<dyn InvocationAdmissionStore>,
        principal_verifier: Arc<dyn ProviderPrincipalVerifier>,
        operations: OperationRegistry,
        evidence: Arc<dyn InvocationEvidenceProvider>,
        clock: Arc<dyn InvocationClock>,
        checkpoint_issuer: Arc<dyn InvocationCheckpointIssuer>,
    ) -> Result<Self, Diagnostic> {
        if !operations.is_frozen() {
            return Err(runtime_error(
                "invocation service requires a deployment-bound frozen operation registry",
            ));
        }
        Ok(Self {
            admissions,
            principal_verifier,
            operations,
            evidence,
            clock,
            checkpoint_issuer,
            events: Arc::new(NoopEventSink),
            pending: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn with_event_sink(mut self, events: Arc<dyn InvocationEventSink>) -> Self {
        self.events = events;
        self
    }

    pub async fn invoke(
        &self,
        request: InvocationRequest,
    ) -> Result<InvocationOutcome, Diagnostic> {
        self.validate(request, None).await
    }

    pub async fn resume(&self, resume: ResumeRequest) -> Result<InvocationOutcome, Diagnostic> {
        let checkpoint = resume.suspension.checkpoint_ref().as_str().to_owned();
        let pending = self.begin_resume(&checkpoint, &resume.suspension)?;
        let outcome = self.validate(resume.request, Some(&pending)).await;
        self.finish_resume(&checkpoint, pending, outcome)
    }

    async fn validate(
        &self,
        request: InvocationRequest,
        resumed: Option<&PendingSuspension>,
    ) -> Result<InvocationOutcome, Diagnostic> {
        let admission = self
            .admissions
            .load(request.admission_id(), request.grant_digest())
            .await?;
        let locked = admission.locked_capability(request.capability())?;
        let descriptor = locked.descriptor();
        validate_locked_semantics(locked)?;

        self.events.record("validate_request");
        request
            .validate_against_admission(admission.grant_set(), descriptor)
            .map_err(|_| {
                capability_error(
                    "invocation request does not match its persisted admission and locked schema",
                )
            })?;

        let grant = admission.effective_grant(request.capability())?;
        if resumed.is_some_and(|pending| {
            pending.snapshot != AdmissionSnapshot::new(&admission, locked, grant)
        }) {
            return Err(authorization_error(
                "persisted admission state changed across suspension",
            ));
        }

        self.events.record("validate_grant");
        let now = self.clock.now();
        validate_grant_against_locked(grant, locked, admission.grant_set().expires_at())?;
        if admission.grant_set().expires_at() <= now || grant.expires_at() <= now {
            return Err(grant_error(
                "capability grant or admission has expired at point of use",
            ));
        }
        if descriptor.effect() > grant.maximum_effect() {
            return Err(authorization_error(
                "locked operation effect exceeds the effective grant",
            ));
        }
        if descriptor.effect() == EffectClassification::ReadOnly
            && (!descriptor.supports_execution_mode(ExecutionMode::Immediate)
                || !grant
                    .execution_modes()
                    .as_set()
                    .contains(&ExecutionMode::Immediate))
        {
            return Err(authorization_error(
                "immediate read execution is not permitted by the effective grant",
            ));
        }

        self.events.record("authenticate");
        let verified = self
            .principal_verifier
            .verify()
            .await
            .map_err(|_| authorization_error("authenticated principal verification failed"))?;
        let (human, workload) = verified.into_parts();
        let principals = correlate_principals(
            human,
            workload.clone(),
            PortableInvocationRefs::new(
                admission.grant_set().actor().clone(),
                admission.grant_set().agent().clone(),
                workload.run_ref().clone(),
                admission.grant_set().task().clone(),
                admission.grant_set().session().clone(),
                admission.grant_set().admission_id().clone(),
                now,
            ),
        )?;
        if principals.admission_ref() != request.admission_id() {
            return Err(authorization_error(
                "authenticated principal admission does not match the invocation",
            ));
        }

        self.events.record("validate_freshness");
        let authorization = self.operations.authorization_backend()?;
        let current_revisions = authorization
            .revisions()
            .await
            .map_err(|_| policy_error("current authority revisions cannot be proven"))?;
        validate_freshness(
            admission.grant_set(),
            grant,
            descriptor.freshness(),
            &current_revisions,
            now,
        )?;

        self.events.record("validate_resource");
        validate_resource(grant, descriptor, request.selected_resource())?;

        let proposal = EffectProposal::try_new(&request, descriptor).map_err(first_diagnostic)?;
        if resumed.is_some_and(|pending| {
            pending.suspension.proposal_digest() != proposal.proposal_digest()
        }) {
            return Err(authorization_error(
                "resumed request does not match the pending effect proposal",
            ));
        }
        self.events.record("validate_evidence");
        let verified_evidence = match self
            .validate_evidence(&request, grant, &principals, &proposal, now)
            .await?
        {
            EvidenceStatus::Complete(evidence) => evidence,
            EvidenceStatus::Missing(kind) if resumed.is_none() => {
                if !descriptor.supports_execution_mode(ExecutionMode::Suspendable)
                    || !grant
                        .execution_modes()
                        .as_set()
                        .contains(&ExecutionMode::Suspendable)
                {
                    return Err(authorization_error(
                        "required evidence is absent and invocation is not suspendable",
                    ));
                }
                let checkpoint_ref = self.checkpoint_issuer.issue(&proposal)?;
                validate_checkpoint_reference(&checkpoint_ref, proposal.proposal_digest())?;
                let suspension = Suspension::try_new(
                    checkpoint_ref,
                    kind,
                    ProtectedEvidenceRequestRef::new(format!(
                        "evidence-request://{}",
                        proposal.proposal_digest()
                    ))
                    .map_err(authorization_error)?,
                    *proposal.proposal_digest(),
                )
                .map_err(authorization_error)?;
                let checkpoint = suspension.checkpoint_ref().as_str().to_owned();
                let mut pending = self.lock_pending();
                if pending.contains_key(&checkpoint) {
                    return Err(authorization_error(
                        "checkpoint issuer returned a pending reference collision",
                    ));
                }
                pending.insert(
                    checkpoint,
                    SuspensionState::Pending(Box::new(PendingSuspension {
                        suspension: suspension.clone(),
                        snapshot: AdmissionSnapshot::new(&admission, locked, grant),
                    })),
                );
                drop(pending);
                return Ok(InvocationOutcome::Suspended {
                    invocation_id: request.invocation_id().clone(),
                    suspension,
                });
            }
            EvidenceStatus::Missing(_) => {
                return Err(evidence_error(
                    "required evidence is still absent at resume",
                ));
            }
        };

        let operation = self.operations.resolve(request.capability())?;
        let context = InvocationContext::try_new(
            principals,
            request.capability().clone(),
            request.selected_resource().clone(),
            request.trace_context().clone(),
            locked.clone(),
            grant.clone(),
            *request.grant_digest(),
            current_revisions.clone(),
        )?;
        let preconditions = request
            .preconditions()
            .iter()
            .map(|(name, value)| {
                Precondition::try_new(name.clone(), serde_json::Value::String(value.clone()))
                    .map_err(precondition_error)
            })
            .collect::<Result<Vec<_>, _>>()?;

        self.events.record("validate_preconditions");
        validate_required_preconditions(grant, descriptor, &preconditions)?;
        operation
            .validate_preconditions(&context, &preconditions)
            .await
            .map_err(|_| precondition_error("operation precondition is missing or stale"))?;

        self.events.record("authorize");
        let authorization_request = InvocationAuthorizationRequest::new(
            context.principals().clone(),
            context.capability().clone(),
            context.selected_resource().clone(),
            *context.grant_digest(),
            context.loaded_authority_revisions().clone(),
        );
        let decision = authorization.check(&authorization_request).await?;
        let dispatch_now = self.clock.now();
        validate_final_point_of_use(
            admission.grant_set(),
            &context,
            &decision,
            &current_revisions,
            &preconditions,
            &verified_evidence,
            dispatch_now,
        )?;

        if descriptor.effect() != EffectClassification::ReadOnly {
            if !descriptor.supports_execution_mode(ExecutionMode::Deferred)
                || !grant
                    .execution_modes()
                    .as_set()
                    .contains(&ExecutionMode::Deferred)
            {
                return Err(capability_error(
                    "effect invocation reached the pre-execution handoff without deferred mode",
                ));
            }
            return Ok(InvocationOutcome::Deferred {
                invocation_id: request.invocation_id().clone(),
            });
        }

        self.events.record("execute");
        match operation
            .execute(&context, request.arguments().clone())
            .await
        {
            Ok(result) => {
                descriptor.validate_output(&result)?;
                Ok(InvocationOutcome::Succeeded {
                    invocation_id: request.invocation_id().clone(),
                    result,
                })
            }
            Err(OperationFailure::Stable(error)) => {
                descriptor.validate_stable_error(&error)?;
                Ok(InvocationOutcome::Failed {
                    invocation_id: request.invocation_id().clone(),
                    error,
                })
            }
            Err(OperationFailure::Diagnostic(diagnostic)) => Err(diagnostic),
        }
    }

    async fn validate_evidence(
        &self,
        request: &InvocationRequest,
        grant: &EffectiveCapabilityGrant,
        principals: &AuthenticatedInvocationContext,
        proposal: &EffectProposal,
        now: Timestamp,
    ) -> Result<EvidenceStatus, Diagnostic> {
        let required = grant.required_evidence();
        let mut verified = Vec::new();
        for (kind, requirement) in [
            (
                EvidenceKind::Confirmation,
                confirmation_requirement(required.confirmation()),
            ),
            (
                EvidenceKind::Approval,
                approval_requirement(required.approval()),
            ),
            (
                EvidenceKind::Consent,
                consent_requirement(required.consent()),
            ),
        ] {
            let Some(requirement) = requirement else {
                continue;
            };
            let Some(reference) = request.evidence_refs().as_map().get(&requirement.kind) else {
                return Ok(EvidenceStatus::Missing(kind));
            };
            if !protected_evidence_reference(reference) {
                return Err(evidence_error(
                    "evidence must use a protected opaque reference",
                ));
            }
            let reference =
                ProtectedEvidenceRequestRef::new(reference.clone()).map_err(evidence_error)?;
            let evidence = self.evidence.resolve(&reference).await?;
            validate_verified_evidence(
                &evidence,
                &reference,
                kind,
                requirement,
                principals,
                request.capability(),
                request.selected_resource(),
                proposal.proposal_digest(),
                now,
            )?;
            verified.push(evidence);
        }
        Ok(EvidenceStatus::Complete(verified))
    }

    fn begin_resume(
        &self,
        checkpoint: &str,
        suspension: &Suspension,
    ) -> Result<PendingSuspension, Diagnostic> {
        let mut states = self.lock_pending();
        let state = states
            .get_mut(checkpoint)
            .ok_or_else(|| authorization_error("suspension checkpoint is not pending"))?;
        match state {
            SuspensionState::Pending(pending) => {
                if pending.suspension != *suspension {
                    return Err(authorization_error(
                        "resume suspension does not exact-match the pending checkpoint",
                    ));
                }
                let owned = (**pending).clone();
                *state = SuspensionState::InFlight;
                Ok(owned)
            }
            SuspensionState::InFlight => Err(authorization_error(
                "suspension checkpoint already has an in-flight resume",
            )),
        }
    }

    fn finish_resume(
        &self,
        checkpoint: &str,
        pending: PendingSuspension,
        outcome: Result<InvocationOutcome, Diagnostic>,
    ) -> Result<InvocationOutcome, Diagnostic> {
        let mut states = self.lock_pending();
        match &outcome {
            Ok(_) => {
                states.remove(checkpoint);
            }
            Err(error) if error.retry != RetryClass::Never => {
                states.insert(
                    checkpoint.to_owned(),
                    SuspensionState::Pending(Box::new(pending)),
                );
            }
            Err(_) => {
                states.remove(checkpoint);
            }
        }
        outcome
    }

    fn lock_pending(&self) -> MutexGuard<'_, BTreeMap<String, SuspensionState>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

enum EvidenceStatus {
    Complete(Vec<VerifiedEvidence>),
    Missing(EvidenceKind),
}

fn validate_checkpoint_reference(
    reference: &kiteframe_contract::CheckpointRef,
    proposal_digest: &Sha256Digest,
) -> Result<(), Diagnostic> {
    let prefix = format!("checkpoint://{proposal_digest}/");
    let Some(entropy) = reference.as_str().strip_prefix(&prefix) else {
        return Err(authorization_error(
            "checkpoint reference is not namespaced by its proposal",
        ));
    };
    if entropy.len() != 64
        || !entropy
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(authorization_error(
            "checkpoint reference lacks 256-bit lowercase hexadecimal entropy",
        ));
    }
    Ok(())
}

fn protected_evidence_reference(reference: &str) -> bool {
    let Some((scheme, opaque)) = reference.split_once("://") else {
        return false;
    };
    matches!(scheme, "evidence" | "vault")
        && !opaque.is_empty()
        && !opaque
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
}

fn validate_locked_semantics(locked: &LockedCapability) -> Result<(), Diagnostic> {
    let wire = serde_json::to_value(locked.descriptor())
        .map_err(|_| capability_error("locked descriptor cannot be canonicalized"))?;
    let object = wire
        .as_object()
        .ok_or_else(|| capability_error("locked descriptor is not an object"))?;
    let field = |name: &str| {
        object
            .get(name)
            .ok_or_else(|| capability_error("locked descriptor omits required semantics"))
    };
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
        safety.insert(name.to_owned(), field(name)?.clone());
    }
    let expected = [
        descriptor_part_digest("input-schema", field("inputSchema")?)?,
        descriptor_part_digest("output-schema", field("outputSchema")?)?,
        descriptor_part_digest("stable-errors", field("stableErrors")?)?,
        descriptor_part_digest("safety-metadata", &serde_json::Value::Object(safety))?,
    ];
    let persisted = [
        *locked.input_schema_digest(),
        *locked.output_schema_digest(),
        *locked.stable_error_set_digest(),
        *locked.safety_metadata_digest(),
    ];
    if expected != persisted {
        return Err(capability_error(
            "locked capability semantic digests do not match its embedded descriptor",
        ));
    }
    Ok(())
}

fn descriptor_part_digest(
    domain: &str,
    value: &serde_json::Value,
) -> Result<Sha256Digest, Diagnostic> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| capability_error("locked descriptor semantics cannot be canonicalized"))?;
    let mut hasher = Sha256::new();
    hasher.update(b"kiteframe.dev/capability-descriptor/");
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(canonical);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

fn confirmation_requirement(requirement: &ConfirmationRequirement) -> Option<&EvidenceRequirement> {
    match requirement {
        ConfirmationRequirement::None => None,
        ConfirmationRequirement::Required { evidence } => Some(evidence),
    }
}

fn approval_requirement(requirement: &ApprovalRequirement) -> Option<&EvidenceRequirement> {
    match requirement {
        ApprovalRequirement::None => None,
        ApprovalRequirement::Required { evidence } => Some(evidence),
    }
}

fn consent_requirement(requirement: &ConsentRequirement) -> Option<&EvidenceRequirement> {
    match requirement {
        ConsentRequirement::None => None,
        ConsentRequirement::Required { evidence } => Some(evidence),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_verified_evidence(
    evidence: &VerifiedEvidence,
    reference: &ProtectedEvidenceRequestRef,
    kind: EvidenceKind,
    requirement: &EvidenceRequirement,
    principals: &AuthenticatedInvocationContext,
    capability: &CapabilityIdentity,
    selected_resource: &NormalizedResourceSelector,
    proposal_digest: &Sha256Digest,
    now: Timestamp,
) -> Result<(), Diagnostic> {
    let subject_matches = match kind {
        EvidenceKind::Confirmation | EvidenceKind::Consent => {
            evidence.principal_ref == principals.human_ref().as_str()
        }
        EvidenceKind::Approval => !evidence.principal_ref.trim().is_empty(),
    };
    if &evidence.reference != reference
        || evidence.evidence_kind != kind
        || evidence.requirement_kind != requirement.kind
        || evidence.issuer.as_deref() != requirement.issuer.as_deref()
        || !subject_matches
        || &evidence.capability != capability
        || &evidence.selected_resource != selected_resource
        || evidence.issued_at > now
        || evidence.expires_at <= now
        || &evidence.proposal_digest != proposal_digest
    {
        return Err(evidence_error(
            "verified evidence does not match the invocation proposal",
        ));
    }
    Ok(())
}

fn validate_grant_against_locked(
    grant: &EffectiveCapabilityGrant,
    locked: &LockedCapability,
    grant_set_expiry: Timestamp,
) -> Result<(), Diagnostic> {
    if grant.capability() != locked.identity()
        || !grant
            .execution_modes()
            .as_set()
            .is_subset(locked.descriptor().execution_modes().as_set())
        || grant.maximum_effect() > locked.descriptor().effect()
        || grant.expires_at() > grant_set_expiry
        || !grant_evidence_at_least_locked(grant, locked)
        || !freshness_at_least(grant.freshness(), locked.descriptor().freshness())
        || locked
            .descriptor()
            .preconditions()
            .iter()
            .filter(|required| required.required)
            .any(|required| !grant.preconditions().contains(required))
    {
        return Err(grant_error(
            "persisted grant exceeds its exact locked capability",
        ));
    }
    Ok(())
}

fn grant_evidence_at_least_locked(
    grant: &EffectiveCapabilityGrant,
    locked: &LockedCapability,
) -> bool {
    let required = grant.required_evidence();
    evidence_requirement_not_weaker(required.confirmation(), locked.descriptor().confirmation())
        && approval_requirement_not_weaker(required.approval(), locked.descriptor().approval())
        && consent_requirement_not_weaker(required.consent(), locked.descriptor().consent())
}

fn evidence_requirement_not_weaker(
    effective: &ConfirmationRequirement,
    locked: &ConfirmationRequirement,
) -> bool {
    matches!(locked, ConfirmationRequirement::None) || effective == locked
}

fn approval_requirement_not_weaker(
    effective: &ApprovalRequirement,
    locked: &ApprovalRequirement,
) -> bool {
    matches!(locked, ApprovalRequirement::None) || effective == locked
}

fn consent_requirement_not_weaker(
    effective: &ConsentRequirement,
    locked: &ConsentRequirement,
) -> bool {
    matches!(locked, ConsentRequirement::None) || effective == locked
}

fn freshness_at_least(
    effective: &kiteframe_contract::FreshnessRequirement,
    locked: &kiteframe_contract::FreshnessRequirement,
) -> bool {
    maximum_not_larger(
        effective.max_admission_age_seconds,
        locked.max_admission_age_seconds,
    ) && maximum_not_larger(
        effective.max_input_age_seconds,
        locked.max_input_age_seconds,
    ) && (!locked.policy_revision_required || effective.policy_revision_required)
}

fn maximum_not_larger(
    effective: Option<std::num::NonZeroU64>,
    locked: Option<std::num::NonZeroU64>,
) -> bool {
    match (effective, locked) {
        (_, None) => true,
        (Some(effective), Some(locked)) => effective <= locked,
        (None, Some(_)) => false,
    }
}

fn validate_freshness(
    grant_set: &CapabilityGrantSet,
    grant: &EffectiveCapabilityGrant,
    descriptor: &kiteframe_contract::FreshnessRequirement,
    current_revisions: &AuthorityRevisionSet,
    now: Timestamp,
) -> Result<(), Diagnostic> {
    if current_revisions != grant_set.authority_revisions() {
        return Err(policy_error(
            "current authority revisions differ from the admitted revision set",
        ));
    }
    let effective = grant.freshness();
    if descriptor.policy_revision_required && !effective.policy_revision_required {
        return Err(policy_error(
            "effective grant cannot prove required policy freshness",
        ));
    }
    if let Some(max_age) = effective.max_admission_age_seconds {
        let age = now
            .unix_seconds()
            .checked_sub(grant_set.issued_at().unix_seconds())
            .ok_or_else(|| policy_error("admission issue time is in the future"))?;
        if age > max_age.get() {
            return Err(policy_error("admission freshness window has expired"));
        }
    }
    if effective.max_input_age_seconds.is_some() {
        return Err(policy_error(
            "invocation request cannot prove required input freshness",
        ));
    }
    Ok(())
}

fn validate_resource(
    grant: &EffectiveCapabilityGrant,
    descriptor: &kiteframe_contract::CapabilityDescriptor,
    selected: &NormalizedResourceSelector,
) -> Result<(), Diagnostic> {
    if selected.as_str().ends_with(":*")
        || !grant
            .resources()
            .iter()
            .any(|allowed| resource_selector_is_subset_of(selected.as_str(), allowed.as_str()))
    {
        return Err(authorization_error(
            "selected resource is not concrete and within the effective grant",
        ));
    }
    let schema = descriptor.resource_selector_schema().as_schema().as_value();
    let compiled = jsonschema::draft202012::options()
        .build(schema)
        .map_err(|_| capability_error("locked resource selector schema is invalid"))?;
    if !compiled.is_valid(&serde_json::Value::String(selected.as_str().to_owned())) {
        return Err(authorization_error(
            "selected resource does not match the locked selector schema",
        ));
    }
    Ok(())
}

fn validate_required_preconditions(
    grant: &EffectiveCapabilityGrant,
    descriptor: &kiteframe_contract::CapabilityDescriptor,
    supplied: &[Precondition],
) -> Result<(), Diagnostic> {
    let complete = grant
        .preconditions()
        .iter()
        .chain(descriptor.preconditions())
        .filter(|required| required.required)
        .all(|required| {
            supplied
                .iter()
                .any(|candidate| candidate.name() == required.name)
        });
    if complete {
        Ok(())
    } else {
        Err(precondition_error(
            "required capability precondition is missing",
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_final_point_of_use(
    grant_set: &CapabilityGrantSet,
    context: &InvocationContext,
    decision: &AuthorizationDecision,
    current_revisions: &AuthorityRevisionSet,
    supplied_preconditions: &[Precondition],
    verified_evidence: &[VerifiedEvidence],
    now: Timestamp,
) -> Result<(), Diagnostic> {
    if grant_set.expires_at() <= now || context.effective_grant().expires_at() <= now {
        return Err(grant_error(
            "capability grant or admission expired before dispatch",
        ));
    }
    if verified_evidence
        .iter()
        .any(|evidence| evidence.issued_at > now || evidence.expires_at <= now)
    {
        return Err(evidence_error("verified evidence expired before dispatch"));
    }
    validate_authorization(
        context,
        decision,
        current_revisions,
        supplied_preconditions,
        now,
    )
}

fn validate_authorization(
    context: &InvocationContext,
    decision: &AuthorizationDecision,
    current_revisions: &AuthorityRevisionSet,
    supplied_preconditions: &[Precondition],
    now: Timestamp,
) -> Result<(), Diagnostic> {
    let AuthorizationDecision::Allow {
        authority_revisions,
        decided_at,
        narrowed_conditions,
        ..
    } = decision
    else {
        return Err(authorization_error(
            "current point-of-use authorization denied",
        ));
    };
    if authority_revisions != current_revisions {
        return Err(policy_error(
            "authorization decision used stale authority revisions",
        ));
    }
    if *decided_at > now {
        return Err(authorization_error(
            "authorization decision time is in the future",
        ));
    }
    validate_narrowed_conditions(context, narrowed_conditions, supplied_preconditions, now)
}

fn validate_narrowed_conditions(
    context: &InvocationContext,
    conditions: &NarrowedAuthorizationConditions,
    supplied_preconditions: &[Precondition],
    now: Timestamp,
) -> Result<(), Diagnostic> {
    if context.principals().expires_at() <= now
        || context.effective_grant().expires_at() <= now
        || conditions.expires_at() <= now
        || !conditions.resources().iter().any(|allowed| {
            resource_selector_is_subset_of(context.selected_resource().as_str(), allowed.as_str())
        })
    {
        return Err(authorization_error(
            "current authorization does not match persisted invocation state",
        ));
    }
    if conditions.required_preconditions().iter().any(|required| {
        !context
            .locked_capability()
            .descriptor()
            .preconditions()
            .iter()
            .chain(context.effective_grant().preconditions())
            .any(|supported| supported.name == required.name && supported.kind == required.kind)
    }) {
        return Err(authorization_error(
            "authorization decision introduced an unknown precondition",
        ));
    }
    if conditions
        .required_preconditions()
        .iter()
        .filter(|required| required.required)
        .any(|required| {
            !supplied_preconditions
                .iter()
                .any(|candidate| candidate.name() == required.name)
        })
    {
        return Err(precondition_error(
            "authorization-required precondition is missing",
        ));
    }
    Ok(())
}

fn first_diagnostic(mut diagnostics: Vec<Diagnostic>) -> Diagnostic {
    if diagnostics.is_empty() {
        capability_error("invocation validation failed without a diagnostic")
    } else {
        diagnostics.remove(0)
    }
}

fn grant_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::AdmissionExpired,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        message.into(),
    )
}

fn authorization_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::InvocationDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        message.into(),
    )
}

fn evidence_error(message: impl Into<String>) -> Diagnostic {
    let mut diagnostic = authorization_error(message);
    diagnostic.retry = RetryClass::AfterUserAction;
    diagnostic
}

fn policy_error(message: impl Into<String>) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::PolicyStale,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        message.into(),
    );
    diagnostic.retry = RetryClass::AfterRefresh;
    diagnostic
}

fn precondition_error(message: impl Into<String>) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::PreconditionMissing,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        message.into(),
    );
    diagnostic.retry = RetryClass::AfterRefresh;
    diagnostic
}

fn capability_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ResultInvalid,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        message.into(),
    )
}

fn runtime_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ComponentUnresolved,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Runtime,
        message.into(),
    )
}
