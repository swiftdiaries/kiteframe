use std::collections::{BTreeMap, BTreeSet};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ApprovalRequirement, CapabilityDescriptor, CapabilityIdentity, CatalogIdentity,
    ConfirmationRequirement, ConsentRequirement, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticSeverity, DiagnosticStage, EffectClassification, ExecutionMode, FreshnessRequirement,
    IdempotencyRequirement, NonEmptySet, PreconditionDescriptor, ResolvedCapabilityRequirement,
    RetryClass, SafeMessage, Sha256Digest,
};

macro_rules! string_ref {
    ($name:ident, $message:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err($message.to_owned());
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
            }
        }
    };
}

string_ref!(AdmissionId, "admission ID is required");
string_ref!(ActorRef, "actor reference is required");
string_ref!(AgentRef, "agent reference is required");
string_ref!(TaskRef, "task reference is required");
string_ref!(SessionRef, "session reference is required");
string_ref!(PolicyRevision, "policy revision is required");
string_ref!(InvocationId, "invocation ID is required");
string_ref!(IdempotencyKey, "idempotency key is required");
string_ref!(NormalizedResourceSelector, "resource selector is required");
string_ref!(CheckpointRef, "suspension checkpoint reference is required");
string_ref!(
    ProtectedEvidenceRequestRef,
    "protected evidence request reference is required"
);

/// A fixed-width, non-secret correlation identifier permitted in W3C baggage.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct BaggageCorrelationId(String);

impl BaggageCorrelationId {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(
                "baggage correlation ID must be 32 lowercase hexadecimal characters".to_owned(),
            );
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BaggageCorrelationId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl JsonSchema for BaggageCorrelationId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "BaggageCorrelationId".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::BaggageCorrelationId").into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "pattern": "^[0-9a-f]{32}$" })
    }
}

/// Unix seconds. Provider adapters obtain time from their deployment rather than from packages.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Timestamp(u64);

impl Timestamp {
    pub const fn new(unix_seconds: u64) -> Self {
        Self(unix_seconds)
    }

    pub const fn unix_seconds(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraceContext {
    #[schemars(regex(pattern = r"^00-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$"))]
    traceparent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 512), regex(pattern = r"^[ -~]+$"))]
    tracestate: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    baggage: BTreeMap<String, BaggageCorrelationId>,
}

impl TraceContext {
    pub const ALLOWED_BAGGAGE_KEYS: &'static [&'static str] = &[
        "kiteframe.agent_id",
        "kiteframe.request_id",
        "kiteframe.session_id",
        "kiteframe.task_id",
    ];

    pub fn try_new(
        traceparent: impl Into<String>,
        tracestate: Option<String>,
        baggage: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let traceparent = traceparent.into();
        if !valid_traceparent(&traceparent) {
            return Err("traceparent must be a valid W3C trace context header".to_owned());
        }
        if tracestate
            .as_deref()
            .is_some_and(|value| !valid_tracestate(value))
        {
            return Err("tracestate must use the canonical W3C subset".to_owned());
        }
        for (key, value) in &baggage {
            if !Self::ALLOWED_BAGGAGE_KEYS.contains(&key.as_str()) {
                return Err("baggage key is not allowlisted".to_owned());
            }
            BaggageCorrelationId::new(value.clone())?;
        }
        Ok(Self {
            traceparent,
            tracestate,
            baggage: baggage
                .into_iter()
                .map(|(key, value)| BaggageCorrelationId::new(value).map(|value| (key, value)))
                .collect::<Result<_, _>>()?,
        })
    }

    pub fn traceparent(&self) -> &str {
        &self.traceparent
    }

    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }

    pub fn baggage(&self) -> &BTreeMap<String, BaggageCorrelationId> {
        &self.baggage
    }
}

impl<'de> Deserialize<'de> for TraceContext {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            traceparent: String,
            #[serde(default)]
            tracestate: Option<String>,
            #[serde(default)]
            baggage: BTreeMap<String, String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::try_new(raw.traceparent, raw.tracestate, raw.baggage).map_err(D::Error::custom)
    }
}

fn valid_traceparent(value: &str) -> bool {
    let mut parts = value.split('-');
    let (Some(version), Some(trace), Some(parent), Some(flags), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    version == "00"
        && trace.len() == 32
        && parent.len() == 16
        && flags.len() == 2
        && [trace, parent, flags]
            .into_iter()
            .all(|part| part.bytes().all(is_lower_hex))
        && trace.bytes().any(|byte| byte != b'0')
        && parent.bytes().any(|byte| byte != b'0')
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn valid_tracestate(value: &str) -> bool {
    if value.is_empty() || value.len() > 512 || !value.is_ascii() {
        return false;
    }

    let mut seen = BTreeSet::new();
    let mut member_count = 0;
    for member in value.split(',') {
        member_count += 1;
        if member_count > 32 || member.trim() != member {
            return false;
        }
        let Some((key, member_value)) = member.split_once('=') else {
            return false;
        };
        if member_value.contains('=')
            || !valid_tracestate_key(key)
            || !valid_tracestate_value(member_value)
            || !seen.insert(key)
        {
            return false;
        }
    }
    true
}

fn valid_tracestate_key(key: &str) -> bool {
    if let Some((tenant, system)) = key.split_once('@') {
        valid_tracestate_key_part(tenant, 241, true) && valid_tracestate_key_part(system, 14, false)
    } else {
        valid_tracestate_key_part(key, 256, false)
    }
}

fn valid_tracestate_key_part(value: &str, max_length: usize, digit_first: bool) -> bool {
    let bytes = value.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    bytes.len() <= max_length
        && (first.is_ascii_lowercase() || (digit_first && first.is_ascii_digit()))
        && bytes.iter().copied().skip(1).all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'*' | b'/')
        })
}

fn valid_tracestate_value(value: &str) -> bool {
    let bytes = value.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    bytes.len() <= 256
        && valid_tracestate_non_space(*first)
        && valid_tracestate_non_space(*last)
        && bytes
            .iter()
            .copied()
            .all(|byte| byte == b' ' || valid_tracestate_non_space(byte))
}

fn valid_tracestate_non_space(byte: u8) -> bool {
    matches!(byte, 0x21..=0x2b | 0x2d..=0x3c | 0x3e..=0x7e)
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct EvidenceReferences(BTreeMap<String, String>);

impl EvidenceReferences {
    /// Accepts opaque reference strings only; evidence payloads never cross this boundary.
    pub fn try_new(values: BTreeMap<String, Value>) -> Result<Self, String> {
        let mut references = BTreeMap::new();
        for (kind, value) in values {
            if kind.trim().is_empty() {
                return Err("evidence reference kind is required".to_owned());
            }
            let Value::String(reference) = value else {
                return Err("evidence must be supplied as an opaque reference".to_owned());
            };
            if reference.trim().is_empty() {
                return Err("evidence reference is required".to_owned());
            }
            references.insert(kind, reference);
        }
        Ok(Self(references))
    }

    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EvidenceReferences {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_new(BTreeMap::<String, Value>::deserialize(deserializer)?)
            .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct DelegationAncestry(#[schemars(extend("uniqueItems" = true))] Vec<AgentRef>);

impl DelegationAncestry {
    pub fn try_new(agents: Vec<AgentRef>) -> Result<Self, String> {
        let unique_agents: BTreeSet<_> = agents.iter().collect();
        if unique_agents.len() != agents.len() {
            return Err("delegation ancestry must not contain duplicate agents".to_owned());
        }
        Ok(Self(agents))
    }

    pub fn agents(&self) -> &[AgentRef] {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    known_catalog_digest: Option<Sha256Digest>,
    trace_context: TraceContext,
}

impl CatalogRequest {
    pub fn new(known_catalog_digest: Option<Sha256Digest>, trace_context: TraceContext) -> Self {
        Self {
            known_catalog_digest,
            trace_context,
        }
    }

    pub fn known_catalog_digest(&self) -> Option<&Sha256Digest> {
        self.known_catalog_digest.as_ref()
    }

    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }
}

impl<'de> Deserialize<'de> for DelegationAncestry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let agents = Vec::<AgentRef>::deserialize(deserializer)?;
        Self::try_new(agents).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestedCapability {
    capability: CapabilityIdentity,
    #[schemars(extend("uniqueItems" = true))]
    resources: Vec<NormalizedResourceSelector>,
}

impl<'de> Deserialize<'de> for RequestedCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            capability: CapabilityIdentity,
            resources: Vec<NormalizedResourceSelector>,
        }

        let raw = Raw::deserialize(deserializer)?;
        Self::try_new(raw.capability, raw.resources).map_err(D::Error::custom)
    }
}

impl RequestedCapability {
    pub fn try_new(
        capability: CapabilityIdentity,
        mut resources: Vec<NormalizedResourceSelector>,
    ) -> Result<Self, String> {
        resources.sort();
        resources.dedup();
        Ok(Self {
            capability,
            resources,
        })
    }

    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }

    pub fn resources(&self) -> &[NormalizedResourceSelector] {
        &self.resources
    }
}

#[derive(Clone, Debug)]
pub struct AdmissionRequestParts {
    pub actor: ActorRef,
    pub agent: AgentRef,
    pub task: TaskRef,
    pub session: SessionRef,
    pub portable_digest: Sha256Digest,
    pub lock_digest: Sha256Digest,
    pub resolved_digest: Sha256Digest,
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
    pub required_capabilities: Vec<RequestedCapability>,
    pub optional_capabilities: Vec<RequestedCapability>,
    pub resolved_requirements: Vec<ResolvedCapabilityRequirement>,
    pub delegation_ancestry: DelegationAncestry,
    pub contextual_facts: BTreeMap<String, String>,
    pub trace_context: TraceContext,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionRequest {
    actor: ActorRef,
    agent: AgentRef,
    task: TaskRef,
    session: SessionRef,
    portable_digest: Sha256Digest,
    lock_digest: Sha256Digest,
    resolved_digest: Sha256Digest,
    catalog_identity: CatalogIdentity,
    catalog_digest: Sha256Digest,
    required_capabilities: Vec<RequestedCapability>,
    optional_capabilities: Vec<RequestedCapability>,
    resolved_requirements: Vec<ResolvedCapabilityRequirement>,
    delegation_ancestry: DelegationAncestry,
    contextual_facts: BTreeMap<String, String>,
    trace_context: TraceContext,
    request_digest: Sha256Digest,
}

impl AdmissionRequest {
    pub fn try_new(mut parts: AdmissionRequestParts) -> Result<Self, Vec<Diagnostic>> {
        normalize_requests(&mut parts.required_capabilities);
        normalize_requests(&mut parts.optional_capabilities);
        let identities: BTreeSet<_> = parts
            .required_capabilities
            .iter()
            .chain(&parts.optional_capabilities)
            .map(|request| request.capability.clone())
            .collect();
        if identities.len() != parts.required_capabilities.len() + parts.optional_capabilities.len()
        {
            return Err(vec![invalid(
                "requested capability versions must be unique",
            )]);
        }
        for request in parts
            .required_capabilities
            .iter()
            .chain(&parts.optional_capabilities)
        {
            let Some(resolved) = parts
                .resolved_requirements
                .iter()
                .find(|resolved| resolved.identity() == &request.capability)
            else {
                return Err(vec![invalid(
                    "requested capability is not a resolved requirement",
                )]);
            };
            if request.resources.iter().any(|requested| {
                !resolved
                    .resources()
                    .iter()
                    .any(|allowed| selector_is_subset_of(requested.as_str(), allowed))
            }) {
                return Err(vec![invalid(
                    "requested resource selector is broader than the resolved requirement",
                )]);
            }
        }
        let request_digest =
            admission_request_digest(&parts).map_err(|message| vec![invalid(message)])?;
        Ok(Self {
            actor: parts.actor,
            agent: parts.agent,
            task: parts.task,
            session: parts.session,
            portable_digest: parts.portable_digest,
            lock_digest: parts.lock_digest,
            resolved_digest: parts.resolved_digest,
            catalog_identity: parts.catalog_identity,
            catalog_digest: parts.catalog_digest,
            required_capabilities: parts.required_capabilities,
            optional_capabilities: parts.optional_capabilities,
            resolved_requirements: parts.resolved_requirements,
            delegation_ancestry: parts.delegation_ancestry,
            contextual_facts: parts.contextual_facts,
            trace_context: parts.trace_context,
            request_digest,
        })
    }

    pub fn required_capabilities(&self) -> &[RequestedCapability] {
        &self.required_capabilities
    }

    pub fn actor(&self) -> &ActorRef {
        &self.actor
    }

    pub fn agent(&self) -> &AgentRef {
        &self.agent
    }

    pub fn task(&self) -> &TaskRef {
        &self.task
    }

    pub fn session(&self) -> &SessionRef {
        &self.session
    }

    pub fn portable_digest(&self) -> &Sha256Digest {
        &self.portable_digest
    }

    pub fn lock_digest(&self) -> &Sha256Digest {
        &self.lock_digest
    }

    pub fn resolved_digest(&self) -> &Sha256Digest {
        &self.resolved_digest
    }

    pub fn delegation_ancestry(&self) -> &DelegationAncestry {
        &self.delegation_ancestry
    }

    pub fn optional_capabilities(&self) -> &[RequestedCapability] {
        &self.optional_capabilities
    }

    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }
    pub fn catalog_identity(&self) -> &CatalogIdentity {
        &self.catalog_identity
    }
    pub fn catalog_digest(&self) -> &Sha256Digest {
        &self.catalog_digest
    }
    pub fn request_digest(&self) -> &Sha256Digest {
        &self.request_digest
    }
}

impl<'de> Deserialize<'de> for AdmissionRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            actor: ActorRef,
            agent: AgentRef,
            task: TaskRef,
            session: SessionRef,
            portable_digest: Sha256Digest,
            lock_digest: Sha256Digest,
            resolved_digest: Sha256Digest,
            catalog_identity: CatalogIdentity,
            catalog_digest: Sha256Digest,
            required_capabilities: Vec<RequestedCapability>,
            optional_capabilities: Vec<RequestedCapability>,
            resolved_requirements: Vec<ResolvedCapabilityRequirement>,
            delegation_ancestry: DelegationAncestry,
            contextual_facts: BTreeMap<String, String>,
            trace_context: TraceContext,
            request_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(deserializer)?;
        let value = Self::try_new(AdmissionRequestParts {
            actor: raw.actor,
            agent: raw.agent,
            task: raw.task,
            session: raw.session,
            portable_digest: raw.portable_digest,
            lock_digest: raw.lock_digest,
            resolved_digest: raw.resolved_digest,
            catalog_identity: raw.catalog_identity,
            catalog_digest: raw.catalog_digest,
            required_capabilities: raw.required_capabilities,
            optional_capabilities: raw.optional_capabilities,
            resolved_requirements: raw.resolved_requirements,
            delegation_ancestry: raw.delegation_ancestry,
            contextual_facts: raw.contextual_facts,
            trace_context: raw.trace_context,
        })
        .map_err(|errors| D::Error::custom(errors[0].message.as_str()))?;
        if value.request_digest != raw.request_digest {
            return Err(D::Error::custom(
                "request digest does not match canonical admission request",
            ));
        }
        Ok(value)
    }
}

fn admission_request_digest(parts: &AdmissionRequestParts) -> Result<Sha256Digest, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Wire<'a> {
        actor: &'a ActorRef,
        agent: &'a AgentRef,
        task: &'a TaskRef,
        session: &'a SessionRef,
        portable_digest: &'a Sha256Digest,
        lock_digest: &'a Sha256Digest,
        resolved_digest: &'a Sha256Digest,
        catalog_identity: &'a CatalogIdentity,
        catalog_digest: &'a Sha256Digest,
        required_capabilities: &'a [RequestedCapability],
        optional_capabilities: &'a [RequestedCapability],
        resolved_requirements: &'a [ResolvedCapabilityRequirement],
        delegation_ancestry: &'a DelegationAncestry,
        contextual_facts: &'a BTreeMap<String, String>,
        trace_context: &'a TraceContext,
    }
    canonical_digest(
        b"kiteframe:admission-request:v1\0",
        &Wire {
            actor: &parts.actor,
            agent: &parts.agent,
            task: &parts.task,
            session: &parts.session,
            portable_digest: &parts.portable_digest,
            lock_digest: &parts.lock_digest,
            resolved_digest: &parts.resolved_digest,
            catalog_identity: &parts.catalog_identity,
            catalog_digest: &parts.catalog_digest,
            required_capabilities: &parts.required_capabilities,
            optional_capabilities: &parts.optional_capabilities,
            resolved_requirements: &parts.resolved_requirements,
            delegation_ancestry: &parts.delegation_ancestry,
            contextual_facts: &parts.contextual_facts,
            trace_context: &parts.trace_context,
        },
    )
}

fn normalize_requests(requests: &mut [RequestedCapability]) {
    requests.sort_by(|left, right| left.capability.cmp(&right.capability));
}

fn selector_is_subset_of(requested: &str, allowed: &str) -> bool {
    requested == allowed
        || allowed
            .strip_suffix(":*")
            .is_some_and(|prefix| requested.starts_with(&format!("{prefix}:")))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequiredEvidence {
    confirmation: ConfirmationRequirement,
    approval: ApprovalRequirement,
    consent: ConsentRequirement,
}

impl RequiredEvidence {
    pub fn new(
        confirmation: ConfirmationRequirement,
        approval: ApprovalRequirement,
        consent: ConsentRequirement,
    ) -> Self {
        Self {
            confirmation,
            approval,
            consent,
        }
    }

    pub fn confirmation(&self) -> &ConfirmationRequirement {
        &self.confirmation
    }
    pub fn approval(&self) -> &ApprovalRequirement {
        &self.approval
    }
    pub fn consent(&self) -> &ConsentRequirement {
        &self.consent
    }
}

#[derive(Clone, Debug)]
pub struct EffectiveCapabilityGrantParts {
    pub capability: CapabilityIdentity,
    pub resources: Vec<NormalizedResourceSelector>,
    pub execution_modes: NonEmptySet<ExecutionMode>,
    pub maximum_effect: EffectClassification,
    pub expires_at: Timestamp,
    pub required_evidence: RequiredEvidence,
    pub freshness: FreshnessRequirement,
    pub preconditions: Vec<PreconditionDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveCapabilityGrant {
    capability: CapabilityIdentity,
    resources: Vec<NormalizedResourceSelector>,
    execution_modes: NonEmptySet<ExecutionMode>,
    maximum_effect: EffectClassification,
    expires_at: Timestamp,
    required_evidence: RequiredEvidence,
    freshness: FreshnessRequirement,
    preconditions: Vec<PreconditionDescriptor>,
}

impl EffectiveCapabilityGrant {
    pub fn try_new(mut parts: EffectiveCapabilityGrantParts) -> Result<Self, String> {
        parts.resources.sort();
        parts.resources.dedup();
        if parts.resources.is_empty() {
            return Err(
                "effective capability grant must have at least one resource selector".to_owned(),
            );
        }
        parts.preconditions.sort();
        parts.preconditions.dedup();
        Ok(Self {
            capability: parts.capability,
            resources: parts.resources,
            execution_modes: parts.execution_modes,
            maximum_effect: parts.maximum_effect,
            expires_at: parts.expires_at,
            required_evidence: parts.required_evidence,
            freshness: parts.freshness,
            preconditions: parts.preconditions,
        })
    }

    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }
    pub fn resources(&self) -> &[NormalizedResourceSelector] {
        &self.resources
    }
    pub fn execution_modes(&self) -> &NonEmptySet<ExecutionMode> {
        &self.execution_modes
    }
    pub fn maximum_effect(&self) -> EffectClassification {
        self.maximum_effect
    }
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
    pub fn required_evidence(&self) -> &RequiredEvidence {
        &self.required_evidence
    }
    pub fn freshness(&self) -> &FreshnessRequirement {
        &self.freshness
    }
    pub fn preconditions(&self) -> &[PreconditionDescriptor] {
        &self.preconditions
    }
}

impl<'de> Deserialize<'de> for EffectiveCapabilityGrant {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            capability: CapabilityIdentity,
            resources: Vec<NormalizedResourceSelector>,
            execution_modes: NonEmptySet<ExecutionMode>,
            maximum_effect: EffectClassification,
            expires_at: Timestamp,
            required_evidence: RequiredEvidence,
            freshness: FreshnessRequirement,
            preconditions: Vec<PreconditionDescriptor>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::try_new(EffectiveCapabilityGrantParts {
            capability: raw.capability,
            resources: raw.resources,
            execution_modes: raw.execution_modes,
            maximum_effect: raw.maximum_effect,
            expires_at: raw.expires_at,
            required_evidence: raw.required_evidence,
            freshness: raw.freshness,
            preconditions: raw.preconditions,
        })
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityRevision {
    source: String,
    revision: String,
}

impl AuthorityRevision {
    pub fn try_new(source: impl Into<String>, revision: impl Into<String>) -> Result<Self, String> {
        let source = source.into();
        let revision = revision.into();
        if source.trim().is_empty() || revision.trim().is_empty() {
            return Err("authority revision source and revision are required".to_owned());
        }
        Ok(Self { source, revision })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

impl<'de> Deserialize<'de> for AuthorityRevision {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            source: String,
            revision: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::try_new(raw.source, raw.revision).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityRevisionSet {
    entries: Vec<AuthorityRevision>,
    authority_revision_digest: Sha256Digest,
}

impl AuthorityRevisionSet {
    pub fn try_new(mut entries: Vec<AuthorityRevision>) -> Result<Self, String> {
        entries.sort();
        if entries
            .windows(2)
            .any(|pair| pair[0].source == pair[1].source)
        {
            return Err("authority revision sources must be unique".to_owned());
        }
        let authority_revision_digest =
            canonical_digest(b"kiteframe:authority-revision-set:v1\0", &entries)?;
        Ok(Self {
            entries,
            authority_revision_digest,
        })
    }

    pub fn entries(&self) -> &[AuthorityRevision] {
        &self.entries
    }
    pub fn authority_revision_digest(&self) -> &Sha256Digest {
        &self.authority_revision_digest
    }
}

impl<'de> Deserialize<'de> for AuthorityRevisionSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            entries: Vec<AuthorityRevision>,
            authority_revision_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(deserializer)?;
        let value = Self::try_new(raw.entries).map_err(D::Error::custom)?;
        if value.authority_revision_digest != raw.authority_revision_digest {
            return Err(D::Error::custom(
                "authority revision digest does not match canonical entries",
            ));
        }
        Ok(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityDenial {
    capability: CapabilityIdentity,
    diagnostic: Diagnostic,
}

impl CapabilityDenial {
    pub fn try_new(capability: CapabilityIdentity, diagnostic: Diagnostic) -> Result<Self, String> {
        if diagnostic.code != DiagnosticCode::AdmissionDenied
            || diagnostic.category != DiagnosticCategory::Authorization
            || !matches!(
                diagnostic.severity,
                DiagnosticSeverity::Warning | DiagnosticSeverity::Error
            )
            || diagnostic.stage != DiagnosticStage::Admit
            || diagnostic.retry != RetryClass::Never
        {
            return Err(
                "capability denial diagnostic must use the admission denial contract".into(),
            );
        }
        Ok(Self {
            capability,
            diagnostic,
        })
    }

    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
}

impl<'de> Deserialize<'de> for CapabilityDenial {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            capability: CapabilityIdentity,
            diagnostic: Diagnostic,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::try_new(raw.capability, raw.diagnostic).map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug)]
pub struct CapabilityGrantSetParts {
    pub admission_id: AdmissionId,
    pub admission_request_digest: Sha256Digest,
    pub actor: ActorRef,
    pub agent: AgentRef,
    pub task: TaskRef,
    pub session: SessionRef,
    pub policy_revision: PolicyRevision,
    pub catalog_identity: CatalogIdentity,
    pub catalog_digest: Sha256Digest,
    pub authority_revisions: AuthorityRevisionSet,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub grants: Vec<EffectiveCapabilityGrant>,
    pub optional_denials: Vec<CapabilityDenial>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityGrantSet {
    admission_id: AdmissionId,
    admission_request_digest: Sha256Digest,
    actor: ActorRef,
    agent: AgentRef,
    task: TaskRef,
    session: SessionRef,
    policy_revision: PolicyRevision,
    catalog_identity: CatalogIdentity,
    catalog_digest: Sha256Digest,
    authority_revisions: AuthorityRevisionSet,
    issued_at: Timestamp,
    expires_at: Timestamp,
    grants: Vec<EffectiveCapabilityGrant>,
    optional_denials: Vec<CapabilityDenial>,
    grant_digest: Sha256Digest,
}

impl CapabilityGrantSet {
    pub fn try_new(mut parts: CapabilityGrantSetParts) -> Result<Self, Vec<Diagnostic>> {
        if parts.expires_at <= parts.issued_at {
            return Err(vec![invalid("grant expiry must be after its issue time")]);
        }
        parts
            .grants
            .sort_by(|left, right| left.capability.cmp(&right.capability));
        if parts
            .grants
            .windows(2)
            .any(|pair| pair[0].capability == pair[1].capability)
        {
            return Err(vec![invalid("capability grant versions must be unique")]);
        }
        parts
            .optional_denials
            .sort_by(|left, right| left.capability.cmp(&right.capability));
        if parts
            .optional_denials
            .windows(2)
            .any(|pair| pair[0].capability == pair[1].capability)
        {
            return Err(vec![invalid(
                "optional capability denial versions must be unique",
            )]);
        }
        let digest = grant_digest(&parts).map_err(|message| vec![invalid(message)])?;
        Ok(Self {
            admission_id: parts.admission_id,
            admission_request_digest: parts.admission_request_digest,
            actor: parts.actor,
            agent: parts.agent,
            task: parts.task,
            session: parts.session,
            policy_revision: parts.policy_revision,
            catalog_identity: parts.catalog_identity,
            catalog_digest: parts.catalog_digest,
            authority_revisions: parts.authority_revisions,
            issued_at: parts.issued_at,
            expires_at: parts.expires_at,
            grants: parts.grants,
            optional_denials: parts.optional_denials,
            grant_digest: digest,
        })
    }

    pub fn validate_against(&self, request: &AdmissionRequest) -> Result<(), Diagnostic> {
        if self.actor != request.actor
            || self.agent != request.agent
            || self.task != request.task
            || self.session != request.session
            || self.admission_request_digest != request.request_digest
            || self.catalog_identity != request.catalog_identity
            || self.catalog_digest != request.catalog_digest
        {
            return Err(result_invalid(
                DiagnosticStage::Admit,
                "capability grant identity does not match its admission request",
            ));
        }

        for requested in &request.required_capabilities {
            if self
                .grants
                .iter()
                .filter(|grant| grant.capability == requested.capability)
                .count()
                != 1
            {
                return Err(grant_invalid(
                    "required capability must have exactly one grant",
                ));
            }
        }
        for requested in &request.optional_capabilities {
            let grants = self
                .grants
                .iter()
                .filter(|grant| grant.capability == requested.capability)
                .count();
            let denials = self
                .optional_denials
                .iter()
                .filter(|denial| denial.capability == requested.capability)
                .count();
            if grants + denials != 1 {
                return Err(grant_invalid(
                    "optional capability must have exactly one grant or denial",
                ));
            }
        }
        if self.optional_denials.iter().any(|denial| {
            !request
                .optional_capabilities
                .iter()
                .any(|requested| requested.capability == denial.capability)
        }) {
            return Err(grant_invalid(
                "capability denial does not match an optional request",
            ));
        }

        for grant in &self.grants {
            let Some(requested) = request
                .required_capabilities
                .iter()
                .chain(&request.optional_capabilities)
                .find(|requested| requested.capability == grant.capability)
            else {
                return Err(result_invalid(
                    DiagnosticStage::Admit,
                    "capability grant exceeds the admission request",
                ));
            };
            if grant.resources.iter().any(|granted| {
                !requested
                    .resources
                    .iter()
                    .any(|requested| selector_is_subset_of(granted.as_str(), requested.as_str()))
            }) {
                return Err(result_invalid(
                    DiagnosticStage::Admit,
                    "capability grant resources exceed the admission request",
                ));
            }
            let Some(resolved) = request
                .resolved_requirements
                .iter()
                .find(|resolved| resolved.identity() == &grant.capability)
            else {
                return Err(grant_invalid("capability grant has no locked descriptor"));
            };
            if !effective_grant_narrows(grant, resolved.descriptor(), self.expires_at) {
                return Err(grant_invalid(
                    "effective capability grant exceeds its locked descriptor",
                ));
            }
        }
        Ok(())
    }

    pub fn admission_id(&self) -> &AdmissionId {
        &self.admission_id
    }
    pub fn admission_request_digest(&self) -> &Sha256Digest {
        &self.admission_request_digest
    }
    pub fn actor(&self) -> &ActorRef {
        &self.actor
    }
    pub fn agent(&self) -> &AgentRef {
        &self.agent
    }
    pub fn task(&self) -> &TaskRef {
        &self.task
    }
    pub fn session(&self) -> &SessionRef {
        &self.session
    }
    pub fn policy_revision(&self) -> &PolicyRevision {
        &self.policy_revision
    }
    pub fn catalog_identity(&self) -> &CatalogIdentity {
        &self.catalog_identity
    }
    pub fn catalog_digest(&self) -> &Sha256Digest {
        &self.catalog_digest
    }
    pub fn authority_revisions(&self) -> &AuthorityRevisionSet {
        &self.authority_revisions
    }
    pub fn issued_at(&self) -> Timestamp {
        self.issued_at
    }
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
    pub fn grants(&self) -> &[EffectiveCapabilityGrant] {
        &self.grants
    }
    pub fn optional_denials(&self) -> &[CapabilityDenial] {
        &self.optional_denials
    }
    pub fn grant_digest(&self) -> &Sha256Digest {
        &self.grant_digest
    }
}

impl<'de> Deserialize<'de> for CapabilityGrantSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            admission_id: AdmissionId,
            admission_request_digest: Sha256Digest,
            actor: ActorRef,
            agent: AgentRef,
            task: TaskRef,
            session: SessionRef,
            policy_revision: PolicyRevision,
            catalog_identity: CatalogIdentity,
            catalog_digest: Sha256Digest,
            authority_revisions: AuthorityRevisionSet,
            issued_at: Timestamp,
            expires_at: Timestamp,
            grants: Vec<EffectiveCapabilityGrant>,
            optional_denials: Vec<CapabilityDenial>,
            grant_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(deserializer)?;
        let value = Self::try_new(CapabilityGrantSetParts {
            admission_id: raw.admission_id,
            admission_request_digest: raw.admission_request_digest,
            actor: raw.actor,
            agent: raw.agent,
            task: raw.task,
            session: raw.session,
            policy_revision: raw.policy_revision,
            catalog_identity: raw.catalog_identity,
            catalog_digest: raw.catalog_digest,
            authority_revisions: raw.authority_revisions,
            issued_at: raw.issued_at,
            expires_at: raw.expires_at,
            grants: raw.grants,
            optional_denials: raw.optional_denials,
        })
        .map_err(|errors| D::Error::custom(errors[0].message.as_str()))?;
        if value.grant_digest != raw.grant_digest {
            return Err(D::Error::custom(
                "grant digest does not match canonical grant set",
            ));
        }
        Ok(value)
    }
}

fn grant_digest(parts: &CapabilityGrantSetParts) -> Result<Sha256Digest, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Wire<'a> {
        admission_id: &'a AdmissionId,
        admission_request_digest: &'a Sha256Digest,
        actor: &'a ActorRef,
        agent: &'a AgentRef,
        task: &'a TaskRef,
        session: &'a SessionRef,
        policy_revision: &'a PolicyRevision,
        catalog_identity: &'a CatalogIdentity,
        catalog_digest: &'a Sha256Digest,
        authority_revisions: &'a AuthorityRevisionSet,
        issued_at: Timestamp,
        expires_at: Timestamp,
        grants: &'a [EffectiveCapabilityGrant],
        optional_denials: &'a [CapabilityDenial],
    }
    canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        &Wire {
            admission_id: &parts.admission_id,
            admission_request_digest: &parts.admission_request_digest,
            actor: &parts.actor,
            agent: &parts.agent,
            task: &parts.task,
            session: &parts.session,
            policy_revision: &parts.policy_revision,
            catalog_identity: &parts.catalog_identity,
            catalog_digest: &parts.catalog_digest,
            authority_revisions: &parts.authority_revisions,
            issued_at: parts.issued_at,
            expires_at: parts.expires_at,
            grants: &parts.grants,
            optional_denials: &parts.optional_denials,
        },
    )
}

fn grant_invalid(message: impl Into<SafeMessage>) -> Diagnostic {
    result_invalid(DiagnosticStage::Admit, message)
}

fn effective_grant_narrows(
    grant: &EffectiveCapabilityGrant,
    descriptor: &CapabilityDescriptor,
    grant_set_expiry: Timestamp,
) -> bool {
    grant
        .execution_modes
        .as_set()
        .is_subset(descriptor.execution_modes().as_set())
        && grant.maximum_effect <= descriptor.effect()
        && grant.expires_at <= grant_set_expiry
        && evidence_not_weaker(
            grant.required_evidence.confirmation(),
            descriptor.confirmation(),
        )
        && approval_not_weaker(grant.required_evidence.approval(), descriptor.approval())
        && consent_not_weaker(grant.required_evidence.consent(), descriptor.consent())
        && freshness_not_weaker(&grant.freshness, descriptor.freshness())
        && descriptor
            .preconditions()
            .iter()
            .filter(|precondition| precondition.required)
            .all(|required| grant.preconditions.contains(required))
}

fn evidence_not_weaker(
    effective: &ConfirmationRequirement,
    locked: &ConfirmationRequirement,
) -> bool {
    match locked {
        ConfirmationRequirement::None => true,
        ConfirmationRequirement::Required { .. } => effective == locked,
    }
}

fn approval_not_weaker(effective: &ApprovalRequirement, locked: &ApprovalRequirement) -> bool {
    match locked {
        ApprovalRequirement::None => true,
        ApprovalRequirement::Required { .. } => effective == locked,
    }
}

fn consent_not_weaker(effective: &ConsentRequirement, locked: &ConsentRequirement) -> bool {
    match locked {
        ConsentRequirement::None => true,
        ConsentRequirement::Required { .. } => effective == locked,
    }
}

fn freshness_not_weaker(effective: &FreshnessRequirement, locked: &FreshnessRequirement) -> bool {
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

fn canonical_digest<T: Serialize>(domain: &[u8], value: &T) -> Result<Sha256Digest, String> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| "value cannot be canonicalized".to_owned())?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(canonical);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

const EFFECT_ARGUMENTS_DIGEST_DOMAIN: &[u8] = b"kiteframe:effect-arguments:v1\0";
const EFFECT_PRECONDITIONS_DIGEST_DOMAIN: &[u8] = b"kiteframe:effect-preconditions:v1\0";
const EFFECT_PROPOSAL_DIGEST_DOMAIN: &[u8] = b"kiteframe:effect-proposal:v1\0";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationRequest {
    invocation_id: InvocationId,
    admission_id: AdmissionId,
    grant_digest: Sha256Digest,
    capability: CapabilityIdentity,
    selected_resource: NormalizedResourceSelector,
    arguments: Value,
    preconditions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<IdempotencyKey>,
    evidence_refs: EvidenceReferences,
    trace_context: TraceContext,
}

impl InvocationRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        invocation_id: InvocationId,
        admission_id: AdmissionId,
        grant_digest: Sha256Digest,
        capability: CapabilityIdentity,
        selected_resource: impl Into<String>,
        arguments: Value,
        preconditions: BTreeMap<String, String>,
        idempotency_key: Option<String>,
        evidence_refs: EvidenceReferences,
        trace_context: TraceContext,
    ) -> Result<Self, String> {
        Ok(Self {
            invocation_id,
            admission_id,
            grant_digest,
            capability,
            selected_resource: NormalizedResourceSelector::new(selected_resource)?,
            arguments,
            preconditions,
            idempotency_key: idempotency_key.map(IdempotencyKey::new).transpose()?,
            evidence_refs,
            trace_context,
        })
    }

    pub fn validate_against(
        &self,
        descriptor: &CapabilityDescriptor,
    ) -> Result<(), Vec<Diagnostic>> {
        if self.capability != *descriptor.identity() {
            return Err(vec![invalid(
                "invocation capability must match its descriptor",
            )]);
        }
        match (descriptor.idempotency(), &self.idempotency_key) {
            (IdempotencyRequirement::None, Some(_)) => {
                return Err(vec![invalid(
                    "idempotency key is forbidden by this capability contract",
                )]);
            }
            (IdempotencyRequirement::Required { .. }, None) => {
                return Err(vec![invalid(
                    "effectful invocation requires an idempotency key",
                )]);
            }
            _ => {}
        }
        let compiled = jsonschema::draft202012::options()
            .build(descriptor.input_schema().as_value())
            .map_err(|_| vec![invalid("capability input schema is invalid")])?;
        if !compiled.is_valid(&self.arguments) {
            return Err(vec![invalid(
                "invocation arguments do not match the capability input schema",
            )]);
        }
        Ok(())
    }

    pub fn validate_admission_correlation(
        &self,
        grant_set: &CapabilityGrantSet,
    ) -> Result<(), Diagnostic> {
        if self.admission_id != grant_set.admission_id
            || self.grant_digest != grant_set.grant_digest
        {
            return Err(result_invalid(
                DiagnosticStage::Invoke,
                "invocation admission correlation does not match its capability grant set",
            ));
        }
        Ok(())
    }

    pub fn validate_against_admission(
        &self,
        grant_set: &CapabilityGrantSet,
        descriptor: &CapabilityDescriptor,
    ) -> Result<(), Vec<Diagnostic>> {
        self.validate_admission_correlation(grant_set)
            .map_err(|diagnostic| vec![diagnostic])?;
        self.validate_against(descriptor)
    }

    pub fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }
    pub fn admission_id(&self) -> &AdmissionId {
        &self.admission_id
    }
    pub fn grant_digest(&self) -> &Sha256Digest {
        &self.grant_digest
    }
    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }
    pub fn selected_resource(&self) -> &NormalizedResourceSelector {
        &self.selected_resource
    }
    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
    pub fn preconditions(&self) -> &BTreeMap<String, String> {
        &self.preconditions
    }
    pub fn idempotency_key(&self) -> Option<&IdempotencyKey> {
        self.idempotency_key.as_ref()
    }
    pub fn evidence_refs(&self) -> &EvidenceReferences {
        &self.evidence_refs
    }
    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectProposal {
    invocation_id: InvocationId,
    admission_id: AdmissionId,
    grant_digest: Sha256Digest,
    capability: CapabilityIdentity,
    selected_resource: NormalizedResourceSelector,
    arguments_digest: Sha256Digest,
    preconditions_digest: Sha256Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<IdempotencyKey>,
    effect: EffectClassification,
    proposal_digest: Sha256Digest,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EffectProposalDigestInput<'a> {
    invocation_id: &'a InvocationId,
    admission_id: &'a AdmissionId,
    grant_digest: &'a Sha256Digest,
    capability: &'a CapabilityIdentity,
    selected_resource: &'a NormalizedResourceSelector,
    arguments_digest: &'a Sha256Digest,
    preconditions_digest: &'a Sha256Digest,
    idempotency_key: &'a Option<IdempotencyKey>,
    effect: EffectClassification,
}

impl EffectProposal {
    pub fn try_new(
        request: &InvocationRequest,
        descriptor: &CapabilityDescriptor,
    ) -> Result<Self, Vec<Diagnostic>> {
        request.validate_against(descriptor)?;
        let arguments_digest =
            canonical_digest(EFFECT_ARGUMENTS_DIGEST_DOMAIN, request.arguments())
                .map_err(|message| vec![invalid(message)])?;
        let preconditions_digest =
            canonical_digest(EFFECT_PRECONDITIONS_DIGEST_DOMAIN, request.preconditions())
                .map_err(|message| vec![invalid(message)])?;
        Self::from_digests(
            request.invocation_id.clone(),
            request.admission_id.clone(),
            request.grant_digest,
            request.capability.clone(),
            request.selected_resource.clone(),
            arguments_digest,
            preconditions_digest,
            request.idempotency_key.clone(),
            descriptor.effect(),
            None,
        )
        .map_err(|message| vec![invalid(message)])
    }

    #[allow(clippy::too_many_arguments)]
    fn from_digests(
        invocation_id: InvocationId,
        admission_id: AdmissionId,
        grant_digest: Sha256Digest,
        capability: CapabilityIdentity,
        selected_resource: NormalizedResourceSelector,
        arguments_digest: Sha256Digest,
        preconditions_digest: Sha256Digest,
        idempotency_key: Option<IdempotencyKey>,
        effect: EffectClassification,
        claimed_digest: Option<Sha256Digest>,
    ) -> Result<Self, String> {
        let proposal_digest = canonical_digest(
            EFFECT_PROPOSAL_DIGEST_DOMAIN,
            &EffectProposalDigestInput {
                invocation_id: &invocation_id,
                admission_id: &admission_id,
                grant_digest: &grant_digest,
                capability: &capability,
                selected_resource: &selected_resource,
                arguments_digest: &arguments_digest,
                preconditions_digest: &preconditions_digest,
                idempotency_key: &idempotency_key,
                effect,
            },
        )?;
        if claimed_digest.is_some_and(|claimed| claimed != proposal_digest) {
            return Err("effect proposal digest does not match its canonical semantics".to_owned());
        }
        Ok(Self {
            invocation_id,
            admission_id,
            grant_digest,
            capability,
            selected_resource,
            arguments_digest,
            preconditions_digest,
            idempotency_key,
            effect,
            proposal_digest,
        })
    }

    pub fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }
    pub fn admission_id(&self) -> &AdmissionId {
        &self.admission_id
    }
    pub fn grant_digest(&self) -> &Sha256Digest {
        &self.grant_digest
    }
    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }
    pub fn selected_resource(&self) -> &NormalizedResourceSelector {
        &self.selected_resource
    }
    pub fn arguments_digest(&self) -> &Sha256Digest {
        &self.arguments_digest
    }
    pub fn preconditions_digest(&self) -> &Sha256Digest {
        &self.preconditions_digest
    }
    pub fn idempotency_key(&self) -> Option<&IdempotencyKey> {
        self.idempotency_key.as_ref()
    }
    pub fn effect(&self) -> EffectClassification {
        self.effect
    }
    pub fn proposal_digest(&self) -> &Sha256Digest {
        &self.proposal_digest
    }
}

impl<'de> Deserialize<'de> for EffectProposal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            invocation_id: InvocationId,
            admission_id: AdmissionId,
            grant_digest: Sha256Digest,
            capability: CapabilityIdentity,
            selected_resource: NormalizedResourceSelector,
            arguments_digest: Sha256Digest,
            preconditions_digest: Sha256Digest,
            #[serde(default)]
            idempotency_key: Option<IdempotencyKey>,
            effect: EffectClassification,
            proposal_digest: Sha256Digest,
        }

        let raw = Raw::deserialize(deserializer)?;
        Self::from_digests(
            raw.invocation_id,
            raw.admission_id,
            raw.grant_digest,
            raw.capability,
            raw.selected_resource,
            raw.arguments_digest,
            raw.preconditions_digest,
            raw.idempotency_key,
            raw.effect,
            Some(raw.proposal_digest),
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusRequest {
    invocation_id: InvocationId,
    trace_context: TraceContext,
}

impl StatusRequest {
    pub fn new(invocation_id: InvocationId, trace_context: TraceContext) -> Self {
        Self {
            invocation_id,
            trace_context,
        }
    }

    pub fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }

    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StableCapabilityError {
    #[schemars(length(min = 1))]
    code: String,
    #[schemars(length(min = 1))]
    category: String,
    retry: RetryClass,
    message: SafeMessage,
}

impl StableCapabilityError {
    pub fn try_new(
        code: impl Into<String>,
        category: impl Into<String>,
        retry: RetryClass,
        message: impl Into<SafeMessage>,
    ) -> Result<Self, String> {
        let code = code.into();
        let category = category.into();
        if code.trim().is_empty() || category.trim().is_empty() {
            return Err("stable capability error code and category are required".to_owned());
        }
        Ok(Self {
            code,
            category,
            retry,
            message: message.into(),
        })
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
    pub fn message(&self) -> &SafeMessage {
        &self.message
    }
}

impl<'de> Deserialize<'de> for StableCapabilityError {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            code: String,
            category: String,
            retry: RetryClass,
            message: SafeMessage,
        }

        let raw = Raw::deserialize(deserializer)?;
        Self::try_new(raw.code, raw.category, raw.retry, raw.message).map_err(D::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Confirmation,
    Approval,
    Consent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Suspension {
    #[schemars(length(min = 1))]
    checkpoint_ref: CheckpointRef,
    evidence_kind: EvidenceKind,
    #[schemars(length(min = 1))]
    evidence_request_ref: ProtectedEvidenceRequestRef,
    proposal_digest: Sha256Digest,
}

impl Suspension {
    pub fn try_new(
        checkpoint_ref: CheckpointRef,
        evidence_kind: EvidenceKind,
        evidence_request_ref: ProtectedEvidenceRequestRef,
        proposal_digest: Sha256Digest,
    ) -> Result<Self, String> {
        Ok(Self {
            checkpoint_ref,
            evidence_kind,
            evidence_request_ref,
            proposal_digest,
        })
    }

    pub fn checkpoint_ref(&self) -> &CheckpointRef {
        &self.checkpoint_ref
    }

    pub fn evidence_kind(&self) -> EvidenceKind {
        self.evidence_kind
    }

    pub fn evidence_request_ref(&self) -> &ProtectedEvidenceRequestRef {
        &self.evidence_request_ref
    }

    pub fn proposal_digest(&self) -> &Sha256Digest {
        &self.proposal_digest
    }
}

/// A diagnostic that proves an unknown result must be reconciled through status first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct StatusFirstDiagnostic(Diagnostic);

impl StatusFirstDiagnostic {
    pub fn try_new(diagnostic: Diagnostic) -> Result<Self, String> {
        validate_status_first(&diagnostic)?;
        Ok(Self(diagnostic))
    }

    pub fn diagnostic(&self) -> &Diagnostic {
        &self.0
    }
}

impl<'de> Deserialize<'de> for StatusFirstDiagnostic {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_new(Diagnostic::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl JsonSchema for StatusFirstDiagnostic {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "StatusFirstDiagnostic".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::StatusFirstDiagnostic").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = generator.subschema_for::<Diagnostic>();
        schema.insert(
            "description".to_owned(),
            "A diagnostic that proves an unknown result must be reconciled through status first."
                .into(),
        );
        schema.insert(
            "properties".to_owned(),
            serde_json::json!({"retry": {"const": "status_first"}}),
        );
        schema
    }
}

fn validate_suspension(
    suspension: &Suspension,
    request: &InvocationRequest,
    descriptor: &CapabilityDescriptor,
) -> Result<(), Diagnostic> {
    descriptor.require_mode(crate::ExecutionMode::Suspendable)?;
    let expected = EffectProposal::try_new(request, descriptor).map_err(|_| {
        result_invalid(
            DiagnosticStage::Invoke,
            "invocation suspension proposal does not match its locked descriptor",
        )
    })?;
    if suspension.proposal_digest() != expected.proposal_digest() {
        return Err(result_invalid(
            DiagnosticStage::Invoke,
            "invocation suspension proposal digest does not match the invocation",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "status", deny_unknown_fields)]
pub enum InvocationOutcome {
    Succeeded {
        invocation_id: InvocationId,
        result: Value,
    },
    Failed {
        invocation_id: InvocationId,
        error: StableCapabilityError,
    },
    Denied {
        invocation_id: InvocationId,
        diagnostic: Diagnostic,
    },
    Suspended {
        invocation_id: InvocationId,
        suspension: Suspension,
    },
    Deferred {
        invocation_id: InvocationId,
    },
    OutcomeUnknown {
        invocation_id: InvocationId,
        diagnostic: StatusFirstDiagnostic,
    },
}

impl InvocationOutcome {
    pub fn outcome_unknown(
        invocation_id: InvocationId,
        diagnostic: Diagnostic,
    ) -> Result<Self, String> {
        Ok(Self::OutcomeUnknown {
            invocation_id,
            diagnostic: StatusFirstDiagnostic::try_new(diagnostic)?,
        })
    }

    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Self::Denied { diagnostic, .. } => Some(diagnostic),
            Self::OutcomeUnknown { diagnostic, .. } => Some(diagnostic.diagnostic()),
            _ => None,
        }
    }

    pub fn invocation_id(&self) -> &InvocationId {
        match self {
            Self::Succeeded { invocation_id, .. }
            | Self::Failed { invocation_id, .. }
            | Self::Denied { invocation_id, .. }
            | Self::Suspended { invocation_id, .. }
            | Self::Deferred { invocation_id }
            | Self::OutcomeUnknown { invocation_id, .. } => invocation_id,
        }
    }

    pub fn validate_against(
        &self,
        request: &InvocationRequest,
        descriptor: &CapabilityDescriptor,
    ) -> Result<(), Diagnostic> {
        validate_response_invocation_id(self.invocation_id(), request.invocation_id())?;
        request.validate_against(descriptor).map_err(|_| {
            result_invalid(
                DiagnosticStage::Invoke,
                "invocation request no longer matches its locked descriptor",
            )
        })?;
        match self {
            Self::Succeeded { result, .. } => descriptor.validate_output(result),
            Self::Failed { error, .. } => descriptor.validate_stable_error(error),
            Self::Deferred { .. } => descriptor.require_mode(crate::ExecutionMode::Deferred),
            Self::Suspended { suspension, .. } => {
                validate_suspension(suspension, request, descriptor)
            }
            Self::Denied { diagnostic, .. } => validate_denial_diagnostic(diagnostic),
            Self::OutcomeUnknown { diagnostic, .. } => {
                validate_status_first(diagnostic.diagnostic()).map_err(|_| {
                    result_invalid(DiagnosticStage::Invoke, "invalid status-first error")
                })
            }
        }
    }

    pub fn validate_against_admission(
        &self,
        request: &InvocationRequest,
        grant_set: &CapabilityGrantSet,
        descriptor: &CapabilityDescriptor,
    ) -> Result<(), Diagnostic> {
        request.validate_admission_correlation(grant_set)?;
        self.validate_against(request, descriptor)
    }
}

impl<'de> Deserialize<'de> for InvocationOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case", tag = "status", deny_unknown_fields)]
        enum Raw {
            Succeeded {
                invocation_id: InvocationId,
                result: Value,
            },
            Failed {
                invocation_id: InvocationId,
                error: StableCapabilityError,
            },
            Denied {
                invocation_id: InvocationId,
                diagnostic: Diagnostic,
            },
            Suspended {
                invocation_id: InvocationId,
                suspension: Suspension,
            },
            Deferred {
                invocation_id: InvocationId,
            },
            OutcomeUnknown {
                invocation_id: InvocationId,
                diagnostic: Diagnostic,
            },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Succeeded {
                invocation_id,
                result,
            } => Ok(Self::Succeeded {
                invocation_id,
                result,
            }),
            Raw::Failed {
                invocation_id,
                error,
            } => Ok(Self::Failed {
                invocation_id,
                error,
            }),
            Raw::Denied {
                invocation_id,
                diagnostic,
            } => Ok(Self::Denied {
                invocation_id,
                diagnostic,
            }),
            Raw::Suspended {
                invocation_id,
                suspension,
            } => Ok(Self::Suspended {
                invocation_id,
                suspension,
            }),
            Raw::Deferred { invocation_id } => Ok(Self::Deferred { invocation_id }),
            Raw::OutcomeUnknown {
                invocation_id,
                diagnostic,
            } => Self::outcome_unknown(invocation_id, diagnostic).map_err(D::Error::custom),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "status", deny_unknown_fields)]
pub enum InvocationStatus {
    Pending {
        invocation_id: InvocationId,
    },
    Suspended {
        invocation_id: InvocationId,
        suspension: Suspension,
    },
    Succeeded {
        invocation_id: InvocationId,
        result: Value,
    },
    Failed {
        invocation_id: InvocationId,
        error: StableCapabilityError,
    },
    Denied {
        invocation_id: InvocationId,
        diagnostic: Diagnostic,
    },
    OutcomeUnknown {
        invocation_id: InvocationId,
        diagnostic: StatusFirstDiagnostic,
    },
}

impl InvocationStatus {
    pub fn outcome_unknown(
        invocation_id: InvocationId,
        diagnostic: Diagnostic,
    ) -> Result<Self, String> {
        Ok(Self::OutcomeUnknown {
            invocation_id,
            diagnostic: StatusFirstDiagnostic::try_new(diagnostic)?,
        })
    }

    pub fn invocation_id(&self) -> &InvocationId {
        match self {
            Self::Pending { invocation_id }
            | Self::Suspended { invocation_id, .. }
            | Self::Succeeded { invocation_id, .. }
            | Self::Failed { invocation_id, .. }
            | Self::Denied { invocation_id, .. }
            | Self::OutcomeUnknown { invocation_id, .. } => invocation_id,
        }
    }

    pub fn validate_invocation_id(&self, invocation_id: &InvocationId) -> Result<(), Diagnostic> {
        validate_response_invocation_id(self.invocation_id(), invocation_id)
    }

    pub fn validate_against(
        &self,
        request: &InvocationRequest,
        descriptor: &CapabilityDescriptor,
    ) -> Result<(), Diagnostic> {
        validate_response_invocation_id(self.invocation_id(), request.invocation_id())?;
        request.validate_against(descriptor).map_err(|_| {
            result_invalid(
                DiagnosticStage::Invoke,
                "invocation request no longer matches its locked descriptor",
            )
        })?;
        match self {
            Self::Pending { .. } => descriptor.require_mode(crate::ExecutionMode::Deferred),
            Self::Suspended { suspension, .. } => {
                validate_suspension(suspension, request, descriptor)
            }
            Self::Succeeded { result, .. } => descriptor.validate_output(result),
            Self::Failed { error, .. } => descriptor.validate_stable_error(error),
            Self::Denied { diagnostic, .. } => validate_denial_diagnostic(diagnostic),
            Self::OutcomeUnknown { diagnostic, .. } => {
                validate_status_first(diagnostic.diagnostic()).map_err(|_| {
                    result_invalid(DiagnosticStage::Invoke, "invalid status-first error")
                })
            }
        }
    }

    pub fn validate_against_admission(
        &self,
        request: &InvocationRequest,
        grant_set: &CapabilityGrantSet,
        descriptor: &CapabilityDescriptor,
    ) -> Result<(), Diagnostic> {
        request.validate_admission_correlation(grant_set)?;
        self.validate_against(request, descriptor)
    }

    pub fn validate_for_status_request(
        &self,
        request: &StatusRequest,
        invocation: &InvocationRequest,
        descriptor: &CapabilityDescriptor,
    ) -> Result<(), Diagnostic> {
        validate_response_invocation_id(self.invocation_id(), request.invocation_id())?;
        validate_response_invocation_id(request.invocation_id(), invocation.invocation_id())?;
        self.validate_against(invocation, descriptor)
    }
}

impl<'de> Deserialize<'de> for InvocationStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case", tag = "status", deny_unknown_fields)]
        enum Raw {
            Pending {
                invocation_id: InvocationId,
            },
            Suspended {
                invocation_id: InvocationId,
                suspension: Suspension,
            },
            Succeeded {
                invocation_id: InvocationId,
                result: Value,
            },
            Failed {
                invocation_id: InvocationId,
                error: StableCapabilityError,
            },
            Denied {
                invocation_id: InvocationId,
                diagnostic: Diagnostic,
            },
            OutcomeUnknown {
                invocation_id: InvocationId,
                diagnostic: Diagnostic,
            },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Pending { invocation_id } => Ok(Self::Pending { invocation_id }),
            Raw::Suspended {
                invocation_id,
                suspension,
            } => Ok(Self::Suspended {
                invocation_id,
                suspension,
            }),
            Raw::Succeeded {
                invocation_id,
                result,
            } => Ok(Self::Succeeded {
                invocation_id,
                result,
            }),
            Raw::Failed {
                invocation_id,
                error,
            } => Ok(Self::Failed {
                invocation_id,
                error,
            }),
            Raw::Denied {
                invocation_id,
                diagnostic,
            } => Ok(Self::Denied {
                invocation_id,
                diagnostic,
            }),
            Raw::OutcomeUnknown {
                invocation_id,
                diagnostic,
            } => Self::outcome_unknown(invocation_id, diagnostic).map_err(D::Error::custom),
        }
    }
}

fn validate_status_first(diagnostic: &Diagnostic) -> Result<(), String> {
    if diagnostic.code != DiagnosticCode::OutcomeUnknown
        || diagnostic.category != DiagnosticCategory::Capability
        || diagnostic.severity != DiagnosticSeverity::Error
        || diagnostic.stage != DiagnosticStage::Invoke
        || diagnostic.retry != RetryClass::StatusFirst
    {
        return Err("outcome_unknown diagnostic must match the canonical control tuple".to_owned());
    }
    Ok(())
}

fn validate_denial_diagnostic(diagnostic: &Diagnostic) -> Result<(), Diagnostic> {
    if diagnostic.code == DiagnosticCode::InvocationDenied
        && diagnostic.category == DiagnosticCategory::Authorization
        && diagnostic.severity == DiagnosticSeverity::Error
        && diagnostic.stage == DiagnosticStage::Invoke
        && diagnostic.retry == RetryClass::Never
    {
        Ok(())
    } else {
        Err(result_invalid(
            DiagnosticStage::Invoke,
            "provider denial does not contain an invocation-denied diagnostic",
        ))
    }
}

fn validate_response_invocation_id(
    actual: &InvocationId,
    expected: &InvocationId,
) -> Result<(), Diagnostic> {
    if actual != expected {
        return Err(result_invalid(
            DiagnosticStage::Invoke,
            "provider response invocation ID does not match the request",
        ));
    }
    Ok(())
}

fn result_invalid(stage: DiagnosticStage, message: impl Into<SafeMessage>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ResultInvalid,
        DiagnosticCategory::Capability,
        stage,
        message,
    )
}

fn invalid(message: impl Into<SafeMessage>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Validate,
        message,
    )
}
