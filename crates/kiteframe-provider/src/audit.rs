use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, CapabilityIdentity, CatalogIdentity, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, EffectClassification, EvidenceReferences,
    IdempotencyKey, InvocationId, NormalizedResourceSelector, RetryClass, SessionRef, Sha256Digest,
    TaskRef, Timestamp,
};
use serde::{Serialize, Serializer};

use crate::{
    DecisionRef, HumanPrincipalRef, RunRef, StatusSafeError, StatusSafeResult, TenantRef,
    WorkloadPrincipalRef,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PreconditionRef(String);

impl PreconditionRef {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("audit precondition reference is required".to_owned());
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! correlation_id {
    ($name:ident, $length:literal, $message:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if value.len() != $length
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                    || value.bytes().all(|byte| byte == b'0')
                {
                    return Err($message.to_owned());
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

correlation_id!(
    TraceId,
    32,
    "audit trace ID must be 32 non-zero lowercase hexadecimal characters"
);
correlation_id!(
    SpanId,
    16,
    "audit span ID must be 16 non-zero lowercase hexadecimal characters"
);

trait OpaqueAuditRef {
    fn audit_ref(&self) -> &str;
}

impl OpaqueAuditRef for TenantRef {
    fn audit_ref(&self) -> &str {
        self.as_str()
    }
}

impl OpaqueAuditRef for HumanPrincipalRef {
    fn audit_ref(&self) -> &str {
        self.as_str()
    }
}

impl OpaqueAuditRef for WorkloadPrincipalRef {
    fn audit_ref(&self) -> &str {
        self.as_str()
    }
}

impl OpaqueAuditRef for RunRef {
    fn audit_ref(&self) -> &str {
        self.as_str()
    }
}

impl OpaqueAuditRef for DecisionRef {
    fn audit_ref(&self) -> &str {
        self.as_str()
    }
}

fn serialize_opaque_ref<T: OpaqueAuditRef, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.audit_ref())
}

/// Complete, credential-free authorization evidence persisted before an effect.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationAuditRecord {
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub tenant_ref: TenantRef,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub human_principal_ref: HumanPrincipalRef,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub workload_principal_ref: WorkloadPrincipalRef,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub run_ref: RunRef,
    pub actor: ActorRef,
    pub agent: AgentRef,
    pub task: TaskRef,
    pub session: SessionRef,
    pub capability: CapabilityIdentity,
    pub resource: NormalizedResourceSelector,
    pub admission_id: AdmissionId,
    pub grant_digest: Sha256Digest,
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
    pub descriptor_digest: Sha256Digest,
    pub authority_revision_digest: Sha256Digest,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub decision_reference: DecisionRef,
    pub invocation_id: InvocationId,
    pub status_id: String,
    pub idempotency_key: IdempotencyKey,
    pub precondition_refs: Vec<PreconditionRef>,
    pub evidence_refs: EvidenceReferences,
    pub proposal_digest: Sha256Digest,
    pub portable_digest: Sha256Digest,
    pub lock_digest: Sha256Digest,
    pub binding_digest: Sha256Digest,
    pub resolved_digest: Sha256Digest,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub intended_effect: EffectClassification,
    pub timestamp: Timestamp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeAuditKind {
    Completion,
    Failure,
    Suspension,
    OutcomeUnknown,
}

/// Credential-free effect outcome linked to its durable write-ahead authorization.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutcomeAuditRecord {
    pub write_ahead_record_id: String,
    pub outcome: OutcomeAuditKind,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub tenant_ref: TenantRef,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub human_principal_ref: HumanPrincipalRef,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub workload_principal_ref: WorkloadPrincipalRef,
    #[serde(serialize_with = "serialize_opaque_ref")]
    pub run_ref: RunRef,
    pub actor: ActorRef,
    pub agent: AgentRef,
    pub task: TaskRef,
    pub session: SessionRef,
    pub capability: CapabilityIdentity,
    pub resource: NormalizedResourceSelector,
    pub admission_id: AdmissionId,
    pub grant_digest: Sha256Digest,
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
    pub descriptor_digest: Sha256Digest,
    pub authority_revision_digest: Sha256Digest,
    pub invocation_id: InvocationId,
    pub status_id: String,
    pub idempotency_key: IdempotencyKey,
    pub proposal_digest: Sha256Digest,
    pub portable_digest: Sha256Digest,
    pub lock_digest: Sha256Digest,
    pub binding_digest: Sha256Digest,
    pub resolved_digest: Sha256Digest,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub intended_effect: EffectClassification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_result: Option<StatusSafeResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_error: Option<StatusSafeError>,
    pub timestamp: Timestamp,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "recordType", content = "record")]
pub enum AuditRecord {
    Authorization(AuthorizationAuditRecord),
    Outcome(OutcomeAuditRecord),
}

impl AuditRecord {
    pub fn partition(&self) -> &str {
        match self {
            Self::Authorization(record) => record.tenant_ref.as_str(),
            Self::Outcome(record) => record.tenant_ref.as_str(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableAuditReceipt {
    partition: String,
    sequence: u64,
    previous_hash: Sha256Digest,
    record_hash: Sha256Digest,
    record_id: String,
}

impl DurableAuditReceipt {
    pub fn try_new(
        partition: impl Into<String>,
        sequence: u64,
        previous_hash: Sha256Digest,
        record_hash: Sha256Digest,
    ) -> Result<Self, String> {
        let partition = partition.into();
        if partition.trim().is_empty()
            || partition
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            || sequence == 0
        {
            return Err("durable audit receipt partition and sequence are invalid".to_owned());
        }
        let record_id = format!("audit://{partition}/{sequence}/{record_hash}");
        Ok(Self {
            partition,
            sequence,
            previous_hash,
            record_hash,
            record_id,
        })
    }

    pub fn partition(&self) -> &str {
        &self.partition
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn previous_hash(&self) -> &Sha256Digest {
        &self.previous_hash
    }

    pub fn record_hash(&self) -> &Sha256Digest {
        &self.record_hash
    }

    pub fn record_id(&self) -> &str {
        &self.record_id
    }
}

#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn append(&self, record: AuditRecord) -> Result<DurableAuditReceipt, Diagnostic>;
}

pub(crate) fn audit_unavailable(message: impl Into<String>) -> Diagnostic {
    let message = message.into();
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::AuditUnavailable,
        DiagnosticCategory::Audit,
        DiagnosticStage::Audit,
        message,
    );
    diagnostic.retry = RetryClass::AfterRefresh;
    diagnostic
}
