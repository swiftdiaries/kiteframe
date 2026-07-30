use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, CapabilityIdentity, CatalogIdentity, Diagnostic, DiagnosticCategory,
    DiagnosticCode, DiagnosticStage, IdempotencyKey, InvocationId, NormalizedResourceSelector,
    ProtectedEvidenceRequestRef, Sha256Digest, StableCapabilityError, StatusRequest, Timestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IdempotencyScopeValue {
    actor: ActorRef,
    capability: CapabilityIdentity,
    normalized_resource: NormalizedResourceSelector,
    semantic_operation: String,
}

impl IdempotencyScopeValue {
    pub fn try_new(
        actor: ActorRef,
        capability: CapabilityIdentity,
        normalized_resource: NormalizedResourceSelector,
        semantic_operation: impl Into<String>,
    ) -> Result<Self, String> {
        let semantic_operation = semantic_operation.into();
        if semantic_operation.trim().is_empty() {
            return Err("semantic operation is required".to_owned());
        }
        Ok(Self {
            actor,
            capability,
            normalized_resource,
            semantic_operation,
        })
    }

    pub fn actor(&self) -> &ActorRef {
        &self.actor
    }

    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }

    pub fn normalized_resource(&self) -> &NormalizedResourceSelector {
        &self.normalized_resource
    }

    pub fn semantic_operation(&self) -> &str {
        &self.semantic_operation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationStatusContext {
    tenant_ref: String,
    human_ref: String,
    workload_ref: String,
    run_ref: String,
    actor_ref: String,
    agent_ref: String,
    task_ref: String,
    session_ref: String,
    admission_ref: String,
}

impl InvocationStatusContext {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        tenant_ref: impl Into<String>,
        human_ref: impl Into<String>,
        workload_ref: impl Into<String>,
        run_ref: impl Into<String>,
        actor_ref: impl Into<String>,
        agent_ref: impl Into<String>,
        task_ref: impl Into<String>,
        session_ref: impl Into<String>,
        admission_ref: impl Into<String>,
    ) -> Result<Self, String> {
        let value = Self {
            tenant_ref: tenant_ref.into(),
            human_ref: human_ref.into(),
            workload_ref: workload_ref.into(),
            run_ref: run_ref.into(),
            actor_ref: actor_ref.into(),
            agent_ref: agent_ref.into(),
            task_ref: task_ref.into(),
            session_ref: session_ref.into(),
            admission_ref: admission_ref.into(),
        };
        if [
            &value.tenant_ref,
            &value.human_ref,
            &value.workload_ref,
            &value.run_ref,
            &value.actor_ref,
            &value.agent_ref,
            &value.task_ref,
            &value.session_ref,
            &value.admission_ref,
        ]
        .into_iter()
        .any(|reference| reference.trim().is_empty())
        {
            return Err("complete authenticated invocation status context is required".to_owned());
        }
        Ok(value)
    }

    pub fn from_authenticated(
        context: &crate::AuthenticatedInvocationContext,
    ) -> Result<Self, String> {
        Self::try_new(
            context.tenant_ref().as_str(),
            context.human_ref().as_str(),
            context.workload_ref().as_str(),
            context.run_ref().as_str(),
            context.actor_ref().as_str(),
            context.agent_ref().as_str(),
            context.task_ref().as_str(),
            context.session_ref().as_str(),
            context.admission_ref().as_str(),
        )
    }

    pub fn tenant_ref(&self) -> &str {
        &self.tenant_ref
    }
    pub fn human_ref(&self) -> &str {
        &self.human_ref
    }
    pub fn workload_ref(&self) -> &str {
        &self.workload_ref
    }
    pub fn run_ref(&self) -> &str {
        &self.run_ref
    }
    pub fn actor_ref(&self) -> &str {
        &self.actor_ref
    }
    pub fn agent_ref(&self) -> &str {
        &self.agent_ref
    }
    pub fn task_ref(&self) -> &str {
        &self.task_ref
    }
    pub fn session_ref(&self) -> &str {
        &self.session_ref
    }
    pub fn admission_ref(&self) -> &str {
        &self.admission_ref
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
pub enum InvocationState {
    Reserved,
    Pending,
    Suspended,
    Succeeded { result: Value },
    Failed { error: StableCapabilityError },
    Denied { diagnostic: Diagnostic },
    OutcomeUnknown,
    Abandoned,
}

impl InvocationState {
    pub const fn status_state(&self) -> StatusState {
        match self {
            Self::Reserved | Self::Pending => StatusState::Pending,
            Self::Suspended => StatusState::Suspended,
            Self::Succeeded { .. } => StatusState::Succeeded,
            Self::Failed { .. } => StatusState::Failed,
            Self::Denied { .. } => StatusState::Denied,
            Self::OutcomeUnknown => StatusState::OutcomeUnknown,
            Self::Abandoned => StatusState::Denied,
        }
    }

    pub fn permits_transition_to(&self, next: &Self) -> bool {
        allowed_transition(self, next)
    }

    pub const fn wire_name(&self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Pending => "pending",
            Self::Suspended => "suspended",
            Self::Succeeded { .. } => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Denied { .. } => "denied",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusState {
    Pending,
    Suspended,
    Succeeded,
    Failed,
    Denied,
    OutcomeUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbandonmentAuthorization {
    authorization_record_id: String,
    authorized_by: String,
}

impl AbandonmentAuthorization {
    pub fn try_new(
        authorization_record_id: impl Into<String>,
        authorized_by: impl Into<String>,
    ) -> Result<Self, String> {
        let value = Self {
            authorization_record_id: authorization_record_id.into(),
            authorized_by: authorized_by.into(),
        };
        if value.authorization_record_id.trim().is_empty() || value.authorized_by.trim().is_empty()
        {
            return Err("explicit abandonment authorization is required".to_owned());
        }
        Ok(value)
    }

    pub fn authorization_record_id(&self) -> &str {
        &self.authorization_record_id
    }

    pub fn authorized_by(&self) -> &str {
        &self.authorized_by
    }
}

#[derive(Clone, Debug)]
pub struct StoredInvocation {
    pub invocation_id: InvocationId,
    pub status_id: String,
    pub scope: IdempotencyScopeValue,
    pub idempotency_key: IdempotencyKey,
    pub request_digest: Sha256Digest,
    pub admission_id: AdmissionId,
    pub grant_digest: Sha256Digest,
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
    pub descriptor_digest: Sha256Digest,
    pub authority_revision_digest: Sha256Digest,
    pub status_context: InvocationStatusContext,
    pub proposal_digest: Sha256Digest,
    pub protected_evidence_refs: Vec<ProtectedEvidenceRequestRef>,
    pub state: InvocationState,
    pub audit_authorization_record_id: Option<String>,
    pub audit_outcome_record_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub retention_until: Timestamp,
    pub abandonment: Option<AbandonmentAuthorization>,
}

impl StoredInvocation {
    pub fn validate_for_storage(&self) -> Result<(), Diagnostic> {
        if self.status_id.trim().is_empty()
            || self.catalog_identity.name.trim().is_empty()
            || self.catalog_identity.revision.trim().is_empty()
            || self.retention_until <= self.created_at
            || self.updated_at < self.created_at
            || self.scope.actor().as_str() != self.status_context.actor_ref()
            || self.admission_id.as_str() != self.status_context.admission_ref()
            || self.state != InvocationState::Reserved
            || self
                .protected_evidence_refs
                .iter()
                .any(|reference| !protected_reference(reference.as_str()))
            || self
                .audit_authorization_record_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            || self
                .audit_outcome_record_id
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            || self.abandonment.is_some()
        {
            return Err(invalid("invalid durable invocation reservation"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationStatus {
    invocation_id: InvocationId,
    status_id: String,
    state: InvocationState,
    request_digest: Sha256Digest,
    grant_digest: Sha256Digest,
    catalog_identity: CatalogIdentity,
    catalog_digest: Sha256Digest,
    descriptor_digest: Sha256Digest,
    authority_revision_digest: Sha256Digest,
    proposal_digest: Sha256Digest,
    audit_authorization_record_id: Option<String>,
    audit_outcome_record_id: Option<String>,
    abandonment_authorization_record_id: Option<String>,
    abandoned_by: Option<String>,
    created_at: Timestamp,
    updated_at: Timestamp,
    retention_until: Timestamp,
}

impl InvocationStatus {
    pub fn from_stored(stored: &StoredInvocation) -> Self {
        Self {
            invocation_id: stored.invocation_id.clone(),
            status_id: stored.status_id.clone(),
            state: stored.state.clone(),
            request_digest: stored.request_digest,
            grant_digest: stored.grant_digest,
            catalog_identity: stored.catalog_identity.clone(),
            catalog_digest: stored.catalog_digest,
            descriptor_digest: stored.descriptor_digest,
            authority_revision_digest: stored.authority_revision_digest,
            proposal_digest: stored.proposal_digest,
            audit_authorization_record_id: stored.audit_authorization_record_id.clone(),
            audit_outcome_record_id: stored.audit_outcome_record_id.clone(),
            abandonment_authorization_record_id: stored
                .abandonment
                .as_ref()
                .map(|value| value.authorization_record_id().to_owned()),
            abandoned_by: stored
                .abandonment
                .as_ref()
                .map(|value| value.authorized_by().to_owned()),
            created_at: stored.created_at,
            updated_at: stored.updated_at,
            retention_until: stored.retention_until,
        }
    }

    pub fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }
    pub fn status_id(&self) -> &str {
        &self.status_id
    }
    pub fn state(&self) -> &InvocationState {
        &self.state
    }
    pub fn status_state(&self) -> StatusState {
        self.state.status_state()
    }
    pub fn request_digest(&self) -> &Sha256Digest {
        &self.request_digest
    }
    pub fn grant_digest(&self) -> &Sha256Digest {
        &self.grant_digest
    }
    pub fn catalog_identity(&self) -> &CatalogIdentity {
        &self.catalog_identity
    }
    pub fn catalog_digest(&self) -> &Sha256Digest {
        &self.catalog_digest
    }
    pub fn descriptor_digest(&self) -> &Sha256Digest {
        &self.descriptor_digest
    }
    pub fn authority_revision_digest(&self) -> &Sha256Digest {
        &self.authority_revision_digest
    }
    pub fn proposal_digest(&self) -> &Sha256Digest {
        &self.proposal_digest
    }
    pub fn audit_authorization_record_id(&self) -> Option<&str> {
        self.audit_authorization_record_id.as_deref()
    }
    pub fn audit_outcome_record_id(&self) -> Option<&str> {
        self.audit_outcome_record_id.as_deref()
    }
    pub fn abandonment_authorization_record_id(&self) -> Option<&str> {
        self.abandonment_authorization_record_id.as_deref()
    }
    pub fn abandoned_by(&self) -> Option<&str> {
        self.abandoned_by.as_deref()
    }
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }
    pub fn updated_at(&self) -> Timestamp {
        self.updated_at
    }
    pub fn retention_until(&self) -> Timestamp {
        self.retention_until
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReservationKind {
    Reserved,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationReservation {
    kind: ReservationKind,
    status: InvocationStatus,
}

impl InvocationReservation {
    pub fn from_stored(kind: ReservationKind, stored: &StoredInvocation) -> Self {
        Self {
            kind,
            status: InvocationStatus::from_stored(stored),
        }
    }

    fn reserved(stored: &StoredInvocation) -> Self {
        Self::from_stored(ReservationKind::Reserved, stored)
    }

    fn existing(stored: &StoredInvocation) -> Self {
        Self::from_stored(ReservationKind::Existing, stored)
    }

    pub fn kind(&self) -> ReservationKind {
        self.kind
    }

    pub fn status(&self) -> &InvocationStatus {
        &self.status
    }
}

#[async_trait]
pub trait InvocationStore: Send + Sync {
    async fn reserve_or_get(
        &self,
        invocation: StoredInvocation,
    ) -> Result<InvocationReservation, Diagnostic>;

    async fn transition(
        &self,
        invocation_id: &InvocationId,
        expected: InvocationState,
        next: InvocationState,
    ) -> Result<(), Diagnostic>;

    async fn status(
        &self,
        request: &StatusRequest,
        context: &InvocationStatusContext,
    ) -> Result<InvocationStatus, Diagnostic>;

    async fn abandon(
        &self,
        invocation_id: &InvocationId,
        context: &InvocationStatusContext,
        authorization: AbandonmentAuthorization,
    ) -> Result<(), Diagnostic>;
}

#[derive(Default)]
pub struct InMemoryInvocationStore {
    records: Mutex<BTreeMap<ReservationKey, StoredInvocation>>,
}

impl InMemoryInvocationStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<ReservationKey, StoredInvocation>> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReservationKey {
    actor: String,
    capability_name: String,
    capability_version: String,
    normalized_resource: String,
    semantic_operation: String,
    idempotency_key: String,
}

impl ReservationKey {
    fn from_invocation(invocation: &StoredInvocation) -> Self {
        Self {
            actor: invocation.scope.actor().as_str().to_owned(),
            capability_name: invocation.scope.capability().name().as_str().to_owned(),
            capability_version: invocation.scope.capability().version().as_str().to_owned(),
            normalized_resource: invocation.scope.normalized_resource().as_str().to_owned(),
            semantic_operation: invocation.scope.semantic_operation().to_owned(),
            idempotency_key: invocation.idempotency_key.as_str().to_owned(),
        }
    }

    fn same_scope(&self, other: &Self) -> bool {
        self.actor == other.actor
            && self.capability_name == other.capability_name
            && self.capability_version == other.capability_version
            && self.normalized_resource == other.normalized_resource
            && self.semantic_operation == other.semantic_operation
    }
}

#[async_trait]
impl InvocationStore for InMemoryInvocationStore {
    async fn reserve_or_get(
        &self,
        invocation: StoredInvocation,
    ) -> Result<InvocationReservation, Diagnostic> {
        invocation.validate_for_storage()?;
        let key = ReservationKey::from_invocation(&invocation);
        let mut records = self.lock();
        records.retain(|_, stored| {
            stored.retention_until > invocation.created_at
                || stored.state.status_state() == StatusState::OutcomeUnknown
        });

        if let Some(existing) = records.get(&key) {
            if existing.status_context != invocation.status_context {
                return Err(unauthorized());
            }
            if existing.request_digest != invocation.request_digest {
                return Err(invalid(
                    "idempotency key is already bound to a different request",
                ));
            }
            return Ok(InvocationReservation::existing(existing));
        }
        if records.iter().any(|(existing_key, stored)| {
            existing_key.same_scope(&key)
                && stored.state.status_state() == StatusState::OutcomeUnknown
        }) {
            return Err(outcome_unknown());
        }
        if records
            .values()
            .any(|stored| stored.invocation_id == invocation.invocation_id)
        {
            return Err(invalid("invocation ID is already reserved"));
        }
        records.insert(key, invocation.clone());
        Ok(InvocationReservation::reserved(&invocation))
    }

    async fn transition(
        &self,
        invocation_id: &InvocationId,
        expected: InvocationState,
        next: InvocationState,
    ) -> Result<(), Diagnostic> {
        if !allowed_transition(&expected, &next) {
            return Err(invalid("invocation state transition is not permitted"));
        }
        let mut records = self.lock();
        let stored = records
            .values_mut()
            .find(|stored| &stored.invocation_id == invocation_id)
            .ok_or_else(not_found)?;
        if stored.state != expected {
            return Err(invalid("invocation state compare-and-swap failed"));
        }
        stored.state = next;
        stored.updated_at = Timestamp::new(stored.updated_at.unix_seconds().saturating_add(1));
        Ok(())
    }

    async fn status(
        &self,
        request: &StatusRequest,
        context: &InvocationStatusContext,
    ) -> Result<InvocationStatus, Diagnostic> {
        let records = self.lock();
        let stored = records
            .values()
            .find(|stored| &stored.invocation_id == request.invocation_id())
            .ok_or_else(not_found)?;
        if &stored.status_context != context {
            return Err(unauthorized());
        }
        Ok(InvocationStatus::from_stored(stored))
    }

    async fn abandon(
        &self,
        invocation_id: &InvocationId,
        context: &InvocationStatusContext,
        authorization: AbandonmentAuthorization,
    ) -> Result<(), Diagnostic> {
        let mut records = self.lock();
        let stored = records
            .values_mut()
            .find(|stored| &stored.invocation_id == invocation_id)
            .ok_or_else(not_found)?;
        if &stored.status_context != context {
            return Err(unauthorized());
        }
        if stored.state != InvocationState::OutcomeUnknown {
            return Err(invalid(
                "only an outcome-unknown invocation can be abandoned",
            ));
        }
        stored.abandonment = Some(authorization);
        stored.state = InvocationState::Abandoned;
        stored.updated_at = Timestamp::new(stored.updated_at.unix_seconds().saturating_add(1));
        Ok(())
    }
}

fn allowed_transition(expected: &InvocationState, next: &InvocationState) -> bool {
    matches!(
        (expected, next),
        (InvocationState::Reserved, InvocationState::Pending)
            | (InvocationState::Reserved, InvocationState::Suspended)
            | (InvocationState::Reserved, InvocationState::Denied { .. })
            | (InvocationState::Reserved, InvocationState::OutcomeUnknown)
            | (InvocationState::Pending, InvocationState::Suspended)
            | (InvocationState::Pending, InvocationState::Succeeded { .. })
            | (InvocationState::Pending, InvocationState::Failed { .. })
            | (InvocationState::Pending, InvocationState::Denied { .. })
            | (InvocationState::Pending, InvocationState::OutcomeUnknown)
            | (InvocationState::Suspended, InvocationState::Pending)
            | (InvocationState::Suspended, InvocationState::Denied { .. })
            | (
                InvocationState::OutcomeUnknown,
                InvocationState::Succeeded { .. }
            )
            | (
                InvocationState::OutcomeUnknown,
                InvocationState::Failed { .. }
            )
            | (
                InvocationState::OutcomeUnknown,
                InvocationState::Denied { .. }
            )
    )
}

fn protected_reference(reference: &str) -> bool {
    reference.split_once("://").is_some_and(|(scheme, opaque)| {
        matches!(scheme, "evidence" | "vault")
            && !opaque.is_empty()
            && !opaque
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    })
}

fn invalid(message: &'static str) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ResultInvalid,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        message,
    )
}

fn outcome_unknown() -> Diagnostic {
    Diagnostic::outcome_unknown(
        "a prior invocation in this idempotency scope has an unknown outcome; query status first",
    )
}

fn unauthorized() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::InvocationDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        "authenticated status context does not match the invocation",
    )
}

fn not_found() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::InvocationDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        "invocation status is unavailable",
    )
}
