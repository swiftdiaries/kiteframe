use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, CapabilityDescriptor, CapabilityIdentity, CatalogIdentity, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, IdempotencyKey, InvocationId,
    NormalizedResourceSelector, ProtectedEvidenceRequestRef, RetryClass, Sha256Digest,
    StableCapabilityError, StatusRequest, Timestamp,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusSafeResult {
    value: Value,
}

impl StatusSafeResult {
    pub fn try_new(value: Value, descriptor: &CapabilityDescriptor) -> Result<Self, String> {
        descriptor
            .validate_output(&value)
            .map_err(|_| "status result does not match the locked output projection".to_owned())?;
        Self::project(value)
    }

    fn project(value: Value) -> Result<Self, String> {
        let fields = value
            .as_object()
            .ok_or_else(|| "status result must be an object projection".to_owned())?;
        let mut projection = serde_json::Map::new();
        if let Some(changed) = fields.get("changed") {
            if !changed.is_boolean() {
                return Err("status result field changed must be boolean".to_owned());
            }
            projection.insert("changed".to_owned(), changed.clone());
        }
        for field in ["affectedCount", "itemCount"] {
            if let Some(count) = fields.get(field) {
                if count.as_u64().is_none() {
                    return Err(format!(
                        "status result field {field} must be unsigned integer"
                    ));
                }
                projection.insert(field.to_owned(), count.clone());
            }
        }
        Ok(Self {
            value: Value::Object(projection),
        })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl<'de> Deserialize<'de> for StatusSafeResult {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            value: Value,
        }
        let raw = Raw::deserialize(deserializer)?;
        let projected = Self::project(raw.value.clone()).map_err(D::Error::custom)?;
        if projected.value != raw.value {
            return Err(D::Error::custom(
                "serialized status result contains non-provider-owned fields",
            ));
        }
        Ok(projected)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusSafeError {
    code: String,
    category: String,
    retry: RetryClass,
    message: String,
}

impl StatusSafeError {
    pub fn try_from_stable(error: &StableCapabilityError) -> Result<Self, String> {
        Self::try_new(error.code(), error.category(), error.retry())
    }

    pub fn try_from_diagnostic(diagnostic: &Diagnostic) -> Result<Self, String> {
        Self::try_new(
            diagnostic.code.as_str(),
            diagnostic_category_name(diagnostic.category),
            diagnostic.retry,
        )
    }

    fn try_new(
        code: impl Into<String>,
        category: impl Into<String>,
        retry: RetryClass,
    ) -> Result<Self, String> {
        let code = code.into();
        let category = category.into();
        let value = Self {
            message: canonical_status_error_message(&category).to_owned(),
            code,
            category,
            retry,
        };
        if !safe_status_error_code(&value.code) || !safe_status_error_category(&value.category) {
            return Err("status error projection contains unsafe content".to_owned());
        }
        Ok(value)
    }

    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn category(&self) -> &str {
        &self.category
    }
    pub fn retry(&self) -> RetryClass {
        self.retry
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl<'de> Deserialize<'de> for StatusSafeError {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            code: String,
            category: String,
            retry: RetryClass,
            message: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        let projected =
            Self::try_new(raw.code, raw.category, raw.retry).map_err(D::Error::custom)?;
        if projected.message != raw.message {
            return Err(D::Error::custom(
                "serialized status error contains a non-canonical message",
            ));
        }
        Ok(projected)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
pub enum InvocationState {
    Reserved,
    Pending,
    Suspended,
    Succeeded { result: StatusSafeResult },
    Failed { error: StatusSafeError },
    Denied { error: StatusSafeError },
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
pub enum TransitionAuditRecord {
    None,
    Authorization(String),
    Outcome(String),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationAuditLinkKind {
    Authorization,
    Outcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationAuditLink {
    kind: InvocationAuditLinkKind,
    record_id: String,
    attached_at: Timestamp,
}

impl InvocationAuditLink {
    pub fn try_new(
        kind: InvocationAuditLinkKind,
        record_id: impl Into<String>,
        attached_at: Timestamp,
    ) -> Result<Self, String> {
        let record_id = record_id.into();
        if record_id.trim().is_empty() {
            return Err("audit record ID is required".to_owned());
        }
        Ok(Self {
            kind,
            record_id,
            attached_at,
        })
    }

    fn from_transition(
        record: &TransitionAuditRecord,
        attached_at: Timestamp,
    ) -> Result<Option<Self>, Diagnostic> {
        let (kind, record_id) = match record {
            TransitionAuditRecord::None => return Ok(None),
            TransitionAuditRecord::Authorization(record_id) => {
                (InvocationAuditLinkKind::Authorization, record_id)
            }
            TransitionAuditRecord::Outcome(record_id) => {
                (InvocationAuditLinkKind::Outcome, record_id)
            }
        };
        Self::try_new(kind, record_id, attached_at)
            .map(Some)
            .map_err(|_| invalid("audit record is invalid"))
    }

    pub fn kind(&self) -> InvocationAuditLinkKind {
        self.kind
    }

    pub fn record_id(&self) -> &str {
        &self.record_id
    }

    pub fn attached_at(&self) -> Timestamp {
        self.attached_at
    }
}

impl<'de> Deserialize<'de> for InvocationAuditLink {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            kind: InvocationAuditLinkKind,
            record_id: String,
            attached_at: Timestamp,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::try_new(raw.kind, raw.record_id, raw.attached_at).map_err(D::Error::custom)
    }
}

impl TransitionAuditRecord {
    fn validate(&self) -> Result<(), Diagnostic> {
        match self {
            Self::None => Ok(()),
            Self::Authorization(record_id) | Self::Outcome(record_id)
                if !record_id.trim().is_empty() =>
            {
                Ok(())
            }
            Self::Authorization(_) | Self::Outcome(_) => {
                Err(invalid("audit record ID is required for state transition"))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationTransition {
    expected: InvocationState,
    next: InvocationState,
    audit_record: TransitionAuditRecord,
}

impl InvocationTransition {
    pub fn try_new(
        expected: InvocationState,
        next: InvocationState,
        audit_record: TransitionAuditRecord,
    ) -> Result<Self, Diagnostic> {
        audit_record.validate()?;
        if !allowed_transition(&expected, &next)
            || !audit_matches_transition(&expected, &next, &audit_record)
        {
            return Err(invalid(
                "invocation state transition and audit record do not correspond",
            ));
        }
        Ok(Self {
            expected,
            next,
            audit_record,
        })
    }

    pub fn expected(&self) -> &InvocationState {
        &self.expected
    }
    pub fn next(&self) -> &InvocationState {
        &self.next
    }
    pub fn audit_record(&self) -> &TransitionAuditRecord {
        &self.audit_record
    }
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
pub struct InvocationReservationInput {
    pub invocation_id: InvocationId,
    pub status_id: String,
    pub scope: IdempotencyScopeValue,
    pub idempotency_key: IdempotencyKey,
    pub request_digest: Sha256Digest,
    pub admission_id: AdmissionId,
    pub grant_digest: Sha256Digest,
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
    pub authority_revision_digest: Sha256Digest,
    pub status_context: InvocationStatusContext,
    pub proposal_digest: Sha256Digest,
    pub protected_evidence_refs: Vec<ProtectedEvidenceRequestRef>,
}

impl InvocationReservationInput {
    fn validate(&self) -> Result<(), Diagnostic> {
        if self.status_id.trim().is_empty()
            || self.catalog_identity.name.trim().is_empty()
            || self.catalog_identity.revision.trim().is_empty()
            || self.scope.actor().as_str() != self.status_context.actor_ref()
            || self.admission_id.as_str() != self.status_context.admission_ref()
            || self
                .protected_evidence_refs
                .iter()
                .any(|reference| !protected_reference(reference.as_str()))
        {
            return Err(invalid("invalid durable invocation reservation"));
        }
        Ok(())
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
    pub audit_links: Vec<InvocationAuditLink>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub retention_until: Timestamp,
    pub abandonment: Option<AbandonmentAuthorization>,
}

impl StoredInvocation {
    pub fn try_reserve(
        input: InvocationReservationInput,
        descriptor: &CapabilityDescriptor,
        created_at: Timestamp,
        retention_until: Timestamp,
    ) -> Result<Self, Diagnostic> {
        input.validate()?;
        let required_seconds = match descriptor.idempotency() {
            kiteframe_contract::IdempotencyRequirement::Required {
                scope: kiteframe_contract::IdempotencyScope::ActorCapabilityResourceOperation,
                retention_seconds,
            } => retention_seconds.get(),
            kiteframe_contract::IdempotencyRequirement::None => {
                return Err(invalid(
                    "durable effect reservation requires a locked idempotency contract",
                ));
            }
        };
        if descriptor.identity() != input.scope.capability()
            || input.scope.semantic_operation() != descriptor.identity().name().as_str()
        {
            return Err(invalid(
                "idempotency scope does not match the locked capability descriptor",
            ));
        }
        let minimum_retention = created_at
            .unix_seconds()
            .checked_add(required_seconds)
            .map(Timestamp::new)
            .ok_or_else(|| invalid("idempotency retention deadline overflows"))?;
        if retention_until < minimum_retention {
            return Err(invalid(
                "retention deadline is shorter than the locked descriptor contract",
            ));
        }
        Ok(Self {
            invocation_id: input.invocation_id,
            status_id: input.status_id,
            scope: input.scope,
            idempotency_key: input.idempotency_key,
            request_digest: input.request_digest,
            admission_id: input.admission_id,
            grant_digest: input.grant_digest,
            catalog_identity: input.catalog_identity,
            catalog_digest: input.catalog_digest,
            descriptor_digest: *descriptor.descriptor_digest(),
            authority_revision_digest: input.authority_revision_digest,
            status_context: input.status_context,
            proposal_digest: input.proposal_digest,
            protected_evidence_refs: input.protected_evidence_refs,
            state: InvocationState::Reserved,
            audit_links: Vec::new(),
            created_at,
            updated_at: created_at,
            retention_until,
            abandonment: None,
        })
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
    audit_links: Vec<InvocationAuditLink>,
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
            audit_links: stored.audit_links.clone(),
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
    pub fn audit_links(&self) -> &[InvocationAuditLink] {
        &self.audit_links
    }
    pub fn audit_authorization_record_id(&self) -> Option<&str> {
        self.audit_links
            .iter()
            .rev()
            .find(|link| link.kind == InvocationAuditLinkKind::Authorization)
            .map(InvocationAuditLink::record_id)
    }
    pub fn audit_outcome_record_id(&self) -> Option<&str> {
        self.audit_links
            .iter()
            .rev()
            .find(|link| link.kind == InvocationAuditLinkKind::Outcome)
            .map(InvocationAuditLink::record_id)
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

    pub fn portable(&self) -> Result<kiteframe_contract::InvocationStatus, Diagnostic> {
        use kiteframe_contract::InvocationStatus as Portable;

        match &self.state {
            InvocationState::Reserved | InvocationState::Pending => Ok(Portable::Pending {
                invocation_id: self.invocation_id.clone(),
            }),
            InvocationState::Suspended => Portable::outcome_unknown(
                self.invocation_id.clone(),
                Diagnostic::outcome_unknown(
                    "durable suspension details are unavailable; query the provider before retrying",
                ),
            )
            .map_err(|_| invalid("portable suspension status projection failed")),
            InvocationState::Succeeded { result } => Ok(Portable::Succeeded {
                invocation_id: self.invocation_id.clone(),
                result: result.value().clone(),
            }),
            InvocationState::Failed { error } => Ok(Portable::Failed {
                invocation_id: self.invocation_id.clone(),
                error: StableCapabilityError::try_new(
                    error.code(),
                    error.category(),
                    error.retry(),
                    error.message(),
                )
                .map_err(|_| invalid("portable stable error projection failed"))?,
            }),
            InvocationState::Denied { .. } | InvocationState::Abandoned => Ok(Portable::Denied {
                invocation_id: self.invocation_id.clone(),
                diagnostic: unauthorized(),
            }),
            InvocationState::OutcomeUnknown => Portable::outcome_unknown(
                self.invocation_id.clone(),
                Diagnostic::outcome_unknown(
                    "effect outcome is uncertain; query status before retrying",
                ),
            )
            .map_err(|_| invalid("portable outcome-unknown projection failed")),
        }
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
        invocation: InvocationReservationInput,
        descriptor: &CapabilityDescriptor,
        retention_until: Timestamp,
    ) -> Result<InvocationReservation, Diagnostic>;

    async fn transition(
        &self,
        invocation_id: &InvocationId,
        transition: InvocationTransition,
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

pub struct InMemoryInvocationStore {
    records: Mutex<BTreeMap<ReservationKey, StoredInvocation>>,
    clock: Arc<dyn InvocationStoreClock>,
}

impl InMemoryInvocationStore {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemInvocationStoreClock))
    }

    pub fn with_clock(clock: Arc<dyn InvocationStoreClock>) -> Self {
        Self {
            records: Mutex::new(BTreeMap::new()),
            clock,
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<ReservationKey, StoredInvocation>> {
        self.records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for InMemoryInvocationStore {
    fn default() -> Self {
        Self::new()
    }
}

pub trait InvocationStoreClock: Send + Sync {
    fn now(&self) -> Timestamp;
}

pub struct SystemInvocationStoreClock;

impl InvocationStoreClock for SystemInvocationStoreClock {
    fn now(&self) -> Timestamp {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Timestamp::new(seconds)
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
        invocation: InvocationReservationInput,
        descriptor: &CapabilityDescriptor,
        retention_until: Timestamp,
    ) -> Result<InvocationReservation, Diagnostic> {
        let now = self.clock.now();
        let invocation =
            StoredInvocation::try_reserve(invocation, descriptor, now, retention_until)?;
        let key = ReservationKey::from_invocation(&invocation);
        let mut records = self.lock();
        records.retain(|_, stored| {
            stored.retention_until > now
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
        transition: InvocationTransition,
    ) -> Result<(), Diagnostic> {
        let now = self.clock.now();
        let mut records = self.lock();
        let stored = records
            .values_mut()
            .find(|stored| &stored.invocation_id == invocation_id)
            .ok_or_else(not_found)?;
        if stored.state != transition.expected {
            return Err(invalid("invocation state compare-and-swap failed"));
        }
        if now < stored.updated_at {
            return Err(invalid("trusted invocation-store clock moved backwards"));
        }
        if let Some(link) = InvocationAuditLink::from_transition(&transition.audit_record, now)? {
            if stored
                .audit_links
                .iter()
                .any(|existing| existing.kind == link.kind && existing.record_id == link.record_id)
            {
                return Err(invalid("audit record is already linked to this invocation"));
            }
            stored.audit_links.push(link);
        }
        stored.state = transition.next;
        stored.updated_at = now;
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

fn audit_matches_transition(
    expected: &InvocationState,
    next: &InvocationState,
    audit: &TransitionAuditRecord,
) -> bool {
    matches!(
        (expected, next, audit),
        (
            InvocationState::Reserved | InvocationState::Suspended,
            InvocationState::Pending,
            TransitionAuditRecord::Authorization(_)
        ) | (
            InvocationState::Pending | InvocationState::OutcomeUnknown,
            InvocationState::Succeeded { .. } | InvocationState::Failed { .. },
            TransitionAuditRecord::Outcome(_)
        ) | (
            InvocationState::Pending,
            InvocationState::OutcomeUnknown,
            TransitionAuditRecord::None | TransitionAuditRecord::Outcome(_)
        ) | (
            InvocationState::Reserved,
            InvocationState::Suspended | InvocationState::Denied { .. },
            TransitionAuditRecord::None
        ) | (
            InvocationState::Pending,
            InvocationState::Suspended | InvocationState::Denied { .. },
            TransitionAuditRecord::None
        ) | (
            InvocationState::Suspended,
            InvocationState::Denied { .. },
            TransitionAuditRecord::None
        ) | (
            InvocationState::OutcomeUnknown,
            InvocationState::Denied { .. },
            TransitionAuditRecord::None
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

fn safe_status_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn safe_status_error_category(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn canonical_status_error_message(category: &str) -> &'static str {
    match category {
        "authorization" => "invocation was denied",
        "runtime" | "audit" => "invocation status is unavailable",
        _ => "capability invocation failed",
    }
}

const fn diagnostic_category_name(category: DiagnosticCategory) -> &'static str {
    match category {
        DiagnosticCategory::Package => "package",
        DiagnosticCategory::Lock => "lock",
        DiagnosticCategory::Catalog => "catalog",
        DiagnosticCategory::Feature => "feature",
        DiagnosticCategory::Authorization => "authorization",
        DiagnosticCategory::Capability => "capability",
        DiagnosticCategory::Audit => "audit",
        DiagnosticCategory::Runtime => "runtime",
    }
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
