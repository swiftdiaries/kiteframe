use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

use async_trait::async_trait;
use kiteframe_contract::{
    ApprovalRequirement, AuthorityRevisionSet, CapabilityGrantSet, CapabilityIdentity,
    ConfirmationRequirement, ConsentRequirement, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, EffectClassification, EffectProposal, EffectiveCapabilityGrant, EvidenceKind,
    EvidenceReferences, EvidenceRequirement, ExecutionMode, IdempotencyRequirement,
    InvocationOutcome, InvocationRequest, LockedCapability, NormalizedResourceSelector,
    ProtectedEvidenceRequestRef, RetryClass, Sha256Digest, StatusRequest, Suspension, Timestamp,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    AuditRecord, AuditSink, AuthenticatedInvocationContext, AuthorizationAuditRecord,
    AuthorizationDecision, IdempotencyScopeValue, InvocationAuthorizationRequest,
    InvocationContext, InvocationReservationInput, InvocationState, InvocationStatusContext,
    InvocationStore, InvocationTransition, NarrowedAuthorizationConditions, OperationFailure,
    OperationRegistry, OutcomeAuditKind, OutcomeAuditRecord, PortableInvocationRefs, Precondition,
    PreconditionRef, ProviderPrincipalVerifier, ReservationKind, SpanId, StatusSafeError,
    StatusSafeResult, TraceId, TransitionAuditRecord, audit::audit_unavailable,
    correlate_principals, resource_selector_is_subset, validate_concrete_resource_selector,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectAuditDigests {
    portable_digest: Sha256Digest,
    lock_digest: Sha256Digest,
    binding_digest: Sha256Digest,
    resolved_digest: Sha256Digest,
}

impl EffectAuditDigests {
    pub fn new(
        portable_digest: Sha256Digest,
        lock_digest: Sha256Digest,
        binding_digest: Sha256Digest,
        resolved_digest: Sha256Digest,
    ) -> Self {
        Self {
            portable_digest,
            lock_digest,
            binding_digest,
            resolved_digest,
        }
    }
}

#[derive(Clone)]
pub struct EffectEnforcementPlane {
    store: Arc<dyn InvocationStore>,
    audit: Arc<dyn AuditSink>,
    digests: EffectAuditDigests,
}

impl EffectEnforcementPlane {
    pub fn new(
        store: Arc<dyn InvocationStore>,
        audit: Arc<dyn AuditSink>,
        digests: EffectAuditDigests,
    ) -> Self {
        Self {
            store,
            audit,
            digests,
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
    effect_enforcement: Option<EffectEnforcementPlane>,
    pending: Mutex<BTreeMap<String, SuspensionState>>,
}

struct ResumeLease<'a> {
    service: &'a InvocationService,
    checkpoint: String,
    pending: Option<PendingSuspension>,
}

impl ResumeLease<'_> {
    fn pending(&self) -> &PendingSuspension {
        self.pending
            .as_ref()
            .expect("resume lease must own its pending suspension until finalized")
    }

    fn finish(
        mut self,
        outcome: Result<InvocationOutcome, Diagnostic>,
    ) -> Result<InvocationOutcome, Diagnostic> {
        let pending = self
            .pending
            .take()
            .expect("resume lease must own its pending suspension until finalized");
        self.service
            .finish_resume(&self.checkpoint, pending, outcome)
    }
}

impl Drop for ResumeLease<'_> {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.service
                .restore_cancelled_resume(&self.checkpoint, pending);
        }
    }
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
            effect_enforcement: None,
            pending: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn with_event_sink(mut self, events: Arc<dyn InvocationEventSink>) -> Self {
        self.events = events;
        self
    }

    pub fn with_effect_enforcement(mut self, enforcement: EffectEnforcementPlane) -> Self {
        self.effect_enforcement = Some(enforcement);
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
        let lease = match self.begin_resume(&checkpoint, &resume.suspension) {
            Ok(lease) => lease,
            Err(error) if error.message.as_str() == "suspension checkpoint is not pending" => {
                self.restore_durable_suspension(&resume).await?;
                self.begin_resume(&checkpoint, &resume.suspension)?
            }
            Err(error) => return Err(error),
        };
        let pending = lease.pending().clone();
        let outcome = self.validate(resume.request, Some(&pending)).await;
        lease.finish(outcome)
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
                self.persist_suspension(
                    &request,
                    &admission,
                    locked,
                    &current_revisions,
                    &principals,
                    &proposal,
                    &suspension,
                )
                .await?;
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
            let Some(enforcement) = &self.effect_enforcement else {
                return Ok(InvocationOutcome::Deferred {
                    invocation_id: request.invocation_id().clone(),
                });
            };
            return self
                .execute_effect(
                    enforcement,
                    &request,
                    &admission,
                    locked,
                    &current_revisions,
                    &context,
                    &proposal,
                    &decision,
                    &verified_evidence,
                    operation.as_ref(),
                    resumed.is_some(),
                )
                .await;
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

    #[allow(clippy::too_many_arguments)]
    async fn execute_effect(
        &self,
        enforcement: &EffectEnforcementPlane,
        request: &InvocationRequest,
        admission: &InvocationAdmission,
        locked: &LockedCapability,
        current_revisions: &AuthorityRevisionSet,
        context: &InvocationContext,
        proposal: &EffectProposal,
        decision: &AuthorizationDecision,
        verified_evidence: &[VerifiedEvidence],
        operation: &dyn crate::CapabilityOperation,
        resumed: bool,
    ) -> Result<InvocationOutcome, Diagnostic> {
        let descriptor = locked.descriptor();
        let idempotency_key = request.idempotency_key().cloned().ok_or_else(|| {
            capability_error("effect invocation is missing its required idempotency key")
        })?;
        let status_context = InvocationStatusContext::from_authenticated(context.principals())
            .map_err(capability_error)?;
        let authority_revision_digest =
            canonical_audit_digest(b"kiteframe:authority-revisions:v1\0", current_revisions)?;
        let status_id = format!("status://{}", request.invocation_id().as_str());
        let retention_until = effect_retention_deadline(descriptor, self.clock.now())?;
        let audit_evidence_refs = validated_audit_evidence_refs(request, verified_evidence)?;
        let protected_evidence_refs = audit_evidence_refs
            .as_map()
            .values()
            .map(|reference| ProtectedEvidenceRequestRef::new(reference.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(capability_error)?;
        let reservation_input = InvocationReservationInput {
            invocation_id: request.invocation_id().clone(),
            status_id: status_id.clone(),
            scope: IdempotencyScopeValue::try_new(
                context.principals().actor_ref().clone(),
                request.capability().clone(),
                request.selected_resource().clone(),
                descriptor.identity().name().as_str(),
            )
            .map_err(capability_error)?,
            idempotency_key: idempotency_key.clone(),
            request_digest: *proposal.proposal_digest(),
            admission_id: request.admission_id().clone(),
            grant_digest: *request.grant_digest(),
            catalog_identity: admission.grant_set().catalog_identity().clone(),
            catalog_digest: *admission.grant_set().catalog_digest(),
            authority_revision_digest,
            status_context,
            proposal_digest: *proposal.proposal_digest(),
            protected_evidence_refs,
        };

        self.events.record("reserve");
        let reservation = enforcement
            .store
            .reserve_or_get(reservation_input, descriptor, retention_until)
            .await?;
        let reservation_state = reservation.status().state().clone();
        let resuming_suspension =
            resumed && matches!(reservation_state, InvocationState::Suspended { .. });
        if reservation.kind() == ReservationKind::Existing && !resuming_suspension {
            return existing_invocation_outcome(
                request.invocation_id().clone(),
                &reservation_state,
            );
        }

        let (trace_id, span_id) = trace_ids(request)?;
        let AuthorizationDecision::Allow { decision_ref, .. } = decision else {
            return Err(authorization_error(
                "validated effect authorization is not an allow decision",
            ));
        };
        let precondition_refs = request
            .preconditions()
            .keys()
            .map(|name| PreconditionRef::new(name.clone()).map_err(capability_error))
            .collect::<Result<Vec<_>, _>>()?;
        let authorization_record = AuthorizationAuditRecord {
            tenant_ref: context.principals().tenant_ref().clone(),
            human_principal_ref: context.principals().human_ref().clone(),
            workload_principal_ref: context.principals().workload_ref().clone(),
            run_ref: context.principals().run_ref().clone(),
            actor: context.principals().actor_ref().clone(),
            agent: context.principals().agent_ref().clone(),
            task: context.principals().task_ref().clone(),
            session: context.principals().session_ref().clone(),
            capability: request.capability().clone(),
            resource: request.selected_resource().clone(),
            admission_id: request.admission_id().clone(),
            grant_digest: *request.grant_digest(),
            catalog_identity: admission.grant_set().catalog_identity().clone(),
            catalog_digest: *admission.grant_set().catalog_digest(),
            descriptor_digest: *locked.descriptor_digest(),
            authority_revision_digest,
            decision_reference: decision_ref.clone(),
            invocation_id: request.invocation_id().clone(),
            status_id: status_id.clone(),
            idempotency_key: idempotency_key.clone(),
            precondition_refs,
            evidence_refs: audit_evidence_refs,
            proposal_digest: *proposal.proposal_digest(),
            portable_digest: enforcement.digests.portable_digest,
            lock_digest: enforcement.digests.lock_digest,
            binding_digest: enforcement.digests.binding_digest,
            resolved_digest: enforcement.digests.resolved_digest,
            trace_id: trace_id.clone(),
            span_id: span_id.clone(),
            intended_effect: descriptor.effect(),
            timestamp: self.clock.now(),
        };
        let authorization_receipt = enforcement
            .audit
            .append(AuditRecord::Authorization(authorization_record))
            .await
            .map_err(|_| audit_unavailable("durable authorization audit append failed"))?;
        self.events.record("audit_authorization");
        enforcement
            .store
            .transition(
                request.invocation_id(),
                InvocationTransition::try_new(
                    if resuming_suspension {
                        reservation_state
                    } else {
                        InvocationState::Reserved
                    },
                    InvocationState::Pending,
                    TransitionAuditRecord::Authorization(
                        authorization_receipt.record_id().to_owned(),
                    ),
                )?,
            )
            .await?;

        self.events.record("execute");
        let execution = operation
            .execute(context, request.arguments().clone())
            .await;
        let prepared = prepare_effect_outcome(request.invocation_id(), descriptor, execution);
        let outcome_record = OutcomeAuditRecord {
            write_ahead_record_id: authorization_receipt.record_id().to_owned(),
            outcome: prepared.audit_kind,
            tenant_ref: context.principals().tenant_ref().clone(),
            human_principal_ref: context.principals().human_ref().clone(),
            workload_principal_ref: context.principals().workload_ref().clone(),
            run_ref: context.principals().run_ref().clone(),
            actor: context.principals().actor_ref().clone(),
            agent: context.principals().agent_ref().clone(),
            task: context.principals().task_ref().clone(),
            session: context.principals().session_ref().clone(),
            capability: request.capability().clone(),
            resource: request.selected_resource().clone(),
            admission_id: request.admission_id().clone(),
            grant_digest: *request.grant_digest(),
            catalog_identity: admission.grant_set().catalog_identity().clone(),
            catalog_digest: *admission.grant_set().catalog_digest(),
            descriptor_digest: *locked.descriptor_digest(),
            authority_revision_digest,
            invocation_id: request.invocation_id().clone(),
            status_id,
            idempotency_key,
            proposal_digest: *proposal.proposal_digest(),
            portable_digest: enforcement.digests.portable_digest,
            lock_digest: enforcement.digests.lock_digest,
            binding_digest: enforcement.digests.binding_digest,
            resolved_digest: enforcement.digests.resolved_digest,
            trace_id,
            span_id,
            intended_effect: descriptor.effect(),
            safe_result: prepared.safe_result.clone(),
            safe_error: prepared.safe_error.clone(),
            timestamp: self.clock.now(),
        };
        let outcome_receipt = match enforcement
            .audit
            .append(AuditRecord::Outcome(outcome_record))
            .await
        {
            Ok(receipt) => receipt,
            Err(_) => {
                let _ = enforcement
                    .store
                    .transition(
                        request.invocation_id(),
                        InvocationTransition::try_new(
                            InvocationState::Pending,
                            InvocationState::OutcomeUnknown,
                            TransitionAuditRecord::None,
                        )?,
                    )
                    .await;
                self.events.record("terminal_status");
                return outcome_unknown(
                    request.invocation_id().clone(),
                    "effect outcome audit append failed; query status before retrying",
                );
            }
        };
        self.events.record("audit_outcome");

        let transition = InvocationTransition::try_new(
            InvocationState::Pending,
            prepared.state.clone(),
            TransitionAuditRecord::Outcome(outcome_receipt.record_id().to_owned()),
        )?;
        if enforcement
            .store
            .transition(request.invocation_id(), transition)
            .await
            .is_err()
        {
            let _ = enforcement
                .store
                .transition(
                    request.invocation_id(),
                    InvocationTransition::try_new(
                        InvocationState::Pending,
                        InvocationState::OutcomeUnknown,
                        TransitionAuditRecord::Outcome(outcome_receipt.record_id().to_owned()),
                    )?,
                )
                .await;
            self.events.record("terminal_status");
            return outcome_unknown(
                request.invocation_id().clone(),
                "effect status transition failed; query status before retrying",
            );
        }
        self.events.record("terminal_status");
        prepared.outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn persist_suspension(
        &self,
        request: &InvocationRequest,
        admission: &InvocationAdmission,
        locked: &LockedCapability,
        current_revisions: &AuthorityRevisionSet,
        principals: &AuthenticatedInvocationContext,
        proposal: &EffectProposal,
        suspension: &Suspension,
    ) -> Result<(), Diagnostic> {
        let descriptor = locked.descriptor();
        if descriptor.effect() == EffectClassification::ReadOnly {
            return Err(capability_error(
                "suspendable invocations require a durable effect contract",
            ));
        }
        let enforcement = self.effect_enforcement.as_ref().ok_or_else(|| {
            audit_unavailable("suspendable effect requires the durable enforcement plane")
        })?;
        let idempotency_key = request.idempotency_key().cloned().ok_or_else(|| {
            capability_error("suspendable effect is missing its required idempotency key")
        })?;
        let authority_revision_digest =
            canonical_audit_digest(b"kiteframe:authority-revisions:v1\0", current_revisions)?;
        let input = InvocationReservationInput {
            invocation_id: request.invocation_id().clone(),
            status_id: format!("status://{}", request.invocation_id().as_str()),
            scope: IdempotencyScopeValue::try_new(
                principals.actor_ref().clone(),
                request.capability().clone(),
                request.selected_resource().clone(),
                descriptor.identity().name().as_str(),
            )
            .map_err(capability_error)?,
            idempotency_key,
            request_digest: *proposal.proposal_digest(),
            admission_id: request.admission_id().clone(),
            grant_digest: *request.grant_digest(),
            catalog_identity: admission.grant_set().catalog_identity().clone(),
            catalog_digest: *admission.grant_set().catalog_digest(),
            authority_revision_digest,
            status_context: InvocationStatusContext::from_authenticated(principals)
                .map_err(capability_error)?,
            proposal_digest: *proposal.proposal_digest(),
            protected_evidence_refs: Vec::new(),
        };
        let reservation = enforcement
            .store
            .reserve_or_get(
                input,
                descriptor,
                effect_retention_deadline(descriptor, self.clock.now())?,
            )
            .await?;
        match reservation.status().state() {
            InvocationState::Suspended {
                suspension: existing,
            } if existing.as_ref() == suspension => return Ok(()),
            InvocationState::Reserved => {}
            _ => {
                return Err(authorization_error(
                    "invocation already has non-suspendable durable state",
                ));
            }
        }
        enforcement
            .store
            .transition(
                request.invocation_id(),
                InvocationTransition::try_new(
                    InvocationState::Reserved,
                    InvocationState::Suspended {
                        suspension: Box::new(suspension.clone()),
                    },
                    TransitionAuditRecord::None,
                )?,
            )
            .await
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
    ) -> Result<ResumeLease<'_>, Diagnostic> {
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
                drop(states);
                Ok(ResumeLease {
                    service: self,
                    checkpoint: checkpoint.to_owned(),
                    pending: Some(owned),
                })
            }
            SuspensionState::InFlight => Err(authorization_error(
                "suspension checkpoint already has an in-flight resume",
            )),
        }
    }

    async fn restore_durable_suspension(&self, resume: &ResumeRequest) -> Result<(), Diagnostic> {
        let enforcement = self
            .effect_enforcement
            .as_ref()
            .ok_or_else(|| authorization_error("suspension has no durable enforcement plane"))?;
        let request = &resume.request;
        let admission = self
            .admissions
            .load(request.admission_id(), request.grant_digest())
            .await?;
        let locked = admission.locked_capability(request.capability())?;
        validate_locked_semantics(locked)?;
        request
            .validate_against_admission(admission.grant_set(), locked.descriptor())
            .map_err(|_| {
                capability_error(
                    "resume request does not match its persisted admission and locked schema",
                )
            })?;
        let grant = admission.effective_grant(request.capability())?;
        let proposal =
            EffectProposal::try_new(request, locked.descriptor()).map_err(first_diagnostic)?;
        if resume.suspension.proposal_digest() != proposal.proposal_digest() {
            return Err(authorization_error(
                "resume suspension does not match the invocation proposal",
            ));
        }

        let now = self.clock.now();
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
        let status_context =
            InvocationStatusContext::from_authenticated(&principals).map_err(capability_error)?;
        let status = enforcement
            .store
            .status(
                &StatusRequest::new(
                    request.invocation_id().clone(),
                    request.trace_context().clone(),
                ),
                &status_context,
            )
            .await?;
        match status.state() {
            InvocationState::Suspended { suspension }
                if suspension.as_ref() == &resume.suspension => {}
            _ => {
                return Err(authorization_error(
                    "durable suspension does not exact-match the resume request",
                ));
            }
        }

        let pending = PendingSuspension {
            suspension: resume.suspension.clone(),
            snapshot: AdmissionSnapshot::new(&admission, locked, grant),
        };
        let checkpoint = resume.suspension.checkpoint_ref().as_str().to_owned();
        let mut states = self.lock_pending();
        match states.get(&checkpoint) {
            None => {
                states.insert(checkpoint, SuspensionState::Pending(Box::new(pending)));
            }
            Some(SuspensionState::Pending(existing))
                if existing.suspension == resume.suspension => {}
            Some(_) => {
                return Err(authorization_error(
                    "suspension checkpoint already has incompatible state",
                ));
            }
        }
        Ok(())
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

    fn restore_cancelled_resume(&self, checkpoint: &str, pending: PendingSuspension) {
        let mut states = self.lock_pending();
        if matches!(states.get(checkpoint), Some(SuspensionState::InFlight)) {
            states.insert(
                checkpoint.to_owned(),
                SuspensionState::Pending(Box::new(pending)),
            );
        }
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

struct PreparedEffectOutcome {
    audit_kind: OutcomeAuditKind,
    state: InvocationState,
    safe_result: Option<StatusSafeResult>,
    safe_error: Option<StatusSafeError>,
    outcome: Result<InvocationOutcome, Diagnostic>,
}

fn prepare_effect_outcome(
    invocation_id: &kiteframe_contract::InvocationId,
    descriptor: &kiteframe_contract::CapabilityDescriptor,
    execution: Result<serde_json::Value, OperationFailure>,
) -> PreparedEffectOutcome {
    match execution {
        Ok(result) => match StatusSafeResult::try_new(result.clone(), descriptor) {
            Ok(safe_result) => PreparedEffectOutcome {
                audit_kind: OutcomeAuditKind::Completion,
                state: InvocationState::Succeeded {
                    result: safe_result.clone(),
                },
                safe_result: Some(safe_result),
                safe_error: None,
                outcome: Ok(InvocationOutcome::Succeeded {
                    invocation_id: invocation_id.clone(),
                    result,
                }),
            },
            Err(_) => prepared_unknown(
                invocation_id,
                capability_error("effect result failed its locked output contract"),
            ),
        },
        Err(OperationFailure::Stable(error)) => {
            if descriptor.validate_stable_error(&error).is_err() {
                return prepared_unknown(
                    invocation_id,
                    capability_error("effect returned an undeclared stable error"),
                );
            }
            match StatusSafeError::try_from_stable(&error) {
                Ok(safe_error) => PreparedEffectOutcome {
                    audit_kind: OutcomeAuditKind::Failure,
                    state: InvocationState::Failed {
                        error: safe_error.clone(),
                    },
                    safe_result: None,
                    safe_error: Some(safe_error),
                    outcome: Ok(InvocationOutcome::Failed {
                        invocation_id: invocation_id.clone(),
                        error,
                    }),
                },
                Err(_) => prepared_unknown(
                    invocation_id,
                    capability_error("effect error could not be projected safely"),
                ),
            }
        }
        Err(OperationFailure::Diagnostic(diagnostic)) => {
            prepared_unknown(invocation_id, diagnostic)
        }
    }
}

fn prepared_unknown(
    invocation_id: &kiteframe_contract::InvocationId,
    diagnostic: Diagnostic,
) -> PreparedEffectOutcome {
    let safe_error = StatusSafeError::try_from_diagnostic(&diagnostic).ok();
    PreparedEffectOutcome {
        audit_kind: OutcomeAuditKind::OutcomeUnknown,
        state: InvocationState::OutcomeUnknown,
        safe_result: None,
        safe_error,
        outcome: outcome_unknown(
            invocation_id.clone(),
            "effect outcome is uncertain; query status before retrying",
        ),
    }
}

fn existing_invocation_outcome(
    invocation_id: kiteframe_contract::InvocationId,
    state: &InvocationState,
) -> Result<InvocationOutcome, Diagnostic> {
    if matches!(state, InvocationState::OutcomeUnknown) {
        outcome_unknown(
            invocation_id,
            "effect outcome is uncertain; query status before retrying",
        )
    } else {
        Ok(InvocationOutcome::Deferred { invocation_id })
    }
}

fn outcome_unknown(
    invocation_id: kiteframe_contract::InvocationId,
    message: &'static str,
) -> Result<InvocationOutcome, Diagnostic> {
    InvocationOutcome::outcome_unknown(invocation_id, Diagnostic::outcome_unknown(message))
        .map_err(capability_error)
}

fn effect_retention_deadline(
    descriptor: &kiteframe_contract::CapabilityDescriptor,
    now: Timestamp,
) -> Result<Timestamp, Diagnostic> {
    let IdempotencyRequirement::Required {
        retention_seconds, ..
    } = descriptor.idempotency()
    else {
        return Err(capability_error(
            "effect descriptor has no durable idempotency contract",
        ));
    };
    now.unix_seconds()
        .checked_add(retention_seconds.get())
        .map(Timestamp::new)
        .ok_or_else(|| capability_error("effect retention deadline overflows"))
}

fn validated_audit_evidence_refs(
    request: &InvocationRequest,
    verified_evidence: &[VerifiedEvidence],
) -> Result<EvidenceReferences, Diagnostic> {
    if request.evidence_refs().as_map().len() != verified_evidence.len() {
        return Err(evidence_error(
            "invocation contains unexpected or unverified evidence references",
        ));
    }
    let mut filtered = BTreeMap::new();
    for evidence in verified_evidence {
        let reference = evidence.reference.as_str();
        if !protected_evidence_reference(reference)
            || request
                .evidence_refs()
                .as_map()
                .get(&evidence.requirement_kind)
                .is_none_or(|supplied| supplied != reference)
            || filtered
                .insert(
                    evidence.requirement_kind.clone(),
                    serde_json::Value::String(reference.to_owned()),
                )
                .is_some()
        {
            return Err(evidence_error(
                "invocation evidence references do not exact-match verified requirements",
            ));
        }
    }
    EvidenceReferences::try_new(filtered).map_err(evidence_error)
}

fn trace_ids(request: &InvocationRequest) -> Result<(TraceId, SpanId), Diagnostic> {
    let mut fields = request.trace_context().traceparent().split('-');
    let _version = fields.next();
    let trace = fields.next().ok_or_else(|| {
        audit_unavailable("validated invocation trace context omitted its trace ID")
    })?;
    let span = fields.next().ok_or_else(|| {
        audit_unavailable("validated invocation trace context omitted its span ID")
    })?;
    let trace_id = TraceId::new(trace).map_err(audit_unavailable)?;
    let span_id = SpanId::new(span).map_err(audit_unavailable)?;
    Ok((trace_id, span_id))
}

fn canonical_audit_digest<T: Serialize>(
    domain: &[u8],
    value: &T,
) -> Result<Sha256Digest, Diagnostic> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| audit_unavailable("audit correlation value cannot be canonicalized"))?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(canonical);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
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
    if validate_concrete_resource_selector(selected.as_str()).is_err()
        || !grant
            .resources()
            .iter()
            .any(|allowed| resource_selector_is_subset(selected.as_str(), allowed.as_str()))
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
            resource_selector_is_subset(context.selected_resource().as_str(), allowed.as_str())
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
