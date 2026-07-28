use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    CapabilityDescriptor, CapabilityIdentity, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, IdempotencyRequirement, ResolvedCapabilityRequirement, RetryClass,
    SafeMessage, Sha256Digest,
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
    traceparent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tracestate: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    baggage: BTreeMap<String, String>,
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
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("tracestate must not be empty when present".to_owned());
        }
        for (key, value) in &baggage {
            if !Self::ALLOWED_BAGGAGE_KEYS.contains(&key.as_str()) {
                return Err("baggage key is not allowlisted".to_owned());
            }
            if value.trim().is_empty() || contains_sensitive_baggage_content(value) {
                return Err("baggage must contain only safe correlation values".to_owned());
            }
        }
        Ok(Self {
            traceparent,
            tracestate,
            baggage,
        })
    }

    pub fn traceparent(&self) -> &str {
        &self.traceparent
    }

    pub fn tracestate(&self) -> Option<&str> {
        self.tracestate.as_deref()
    }

    pub fn baggage(&self) -> &BTreeMap<String, String> {
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
    version.len() == 2
        && trace.len() == 32
        && parent.len() == 16
        && flags.len() == 2
        && [version, trace, parent, flags]
            .into_iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && trace.bytes().any(|byte| byte != b'0')
        && parent.bytes().any(|byte| byte != b'0')
}

fn contains_sensitive_baggage_content(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "credential",
        "authorization",
        "prompt",
        "argument",
        "result",
        "tuple",
    ]
    .iter()
    .any(|forbidden| value.contains(forbidden))
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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct DelegationAncestry(Vec<AgentRef>);

impl DelegationAncestry {
    pub fn try_new(mut agents: Vec<AgentRef>) -> Result<Self, String> {
        let original = agents.len();
        agents.sort();
        agents.dedup();
        if agents.len() != original {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestedCapability {
    capability: CapabilityIdentity,
    resources: Vec<NormalizedResourceSelector>,
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
    pub required_capabilities: Vec<RequestedCapability>,
    pub optional_capabilities: Vec<RequestedCapability>,
    pub resolved_requirements: Vec<ResolvedCapabilityRequirement>,
    pub delegation_ancestry: DelegationAncestry,
    pub contextual_facts: BTreeMap<String, String>,
    pub trace_context: TraceContext,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmissionRequest {
    actor: ActorRef,
    agent: AgentRef,
    task: TaskRef,
    session: SessionRef,
    portable_digest: Sha256Digest,
    lock_digest: Sha256Digest,
    resolved_digest: Sha256Digest,
    required_capabilities: Vec<RequestedCapability>,
    optional_capabilities: Vec<RequestedCapability>,
    delegation_ancestry: DelegationAncestry,
    contextual_facts: BTreeMap<String, String>,
    trace_context: TraceContext,
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
                .find(|resolved| resolved.identity == request.capability)
            else {
                return Err(vec![invalid(
                    "requested capability is not a resolved requirement",
                )]);
            };
            if request.resources.iter().any(|requested| {
                !resolved
                    .resources
                    .iter()
                    .any(|allowed| selector_is_subset_of(requested.as_str(), allowed))
            }) {
                return Err(vec![invalid(
                    "requested resource selector is broader than the resolved requirement",
                )]);
            }
        }
        Ok(Self {
            actor: parts.actor,
            agent: parts.agent,
            task: parts.task,
            session: parts.session,
            portable_digest: parts.portable_digest,
            lock_digest: parts.lock_digest,
            resolved_digest: parts.resolved_digest,
            required_capabilities: parts.required_capabilities,
            optional_capabilities: parts.optional_capabilities,
            delegation_ancestry: parts.delegation_ancestry,
            contextual_facts: parts.contextual_facts,
            trace_context: parts.trace_context,
        })
    }

    pub fn required_capabilities(&self) -> &[RequestedCapability] {
        &self.required_capabilities
    }

    pub fn optional_capabilities(&self) -> &[RequestedCapability] {
        &self.optional_capabilities
    }

    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }
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

#[derive(Clone, Debug)]
pub struct CapabilityGrantParts {
    pub capability: CapabilityIdentity,
    pub resources: Vec<NormalizedResourceSelector>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityGrant {
    capability: CapabilityIdentity,
    resources: Vec<NormalizedResourceSelector>,
}

impl CapabilityGrant {
    pub fn try_new(mut parts: CapabilityGrantParts) -> Result<Self, String> {
        parts.resources.sort();
        parts.resources.dedup();
        if parts.resources.is_empty() {
            return Err("capability grant must have at least one resource selector".to_owned());
        }
        Ok(Self {
            capability: parts.capability,
            resources: parts.resources,
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
pub struct CapabilityGrantSetParts {
    pub admission_id: AdmissionId,
    pub actor: ActorRef,
    pub agent: AgentRef,
    pub task: TaskRef,
    pub session: SessionRef,
    pub policy_revision: PolicyRevision,
    pub catalog_digest: Sha256Digest,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub grants: Vec<CapabilityGrant>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityGrantSet {
    admission_id: AdmissionId,
    actor: ActorRef,
    agent: AgentRef,
    task: TaskRef,
    session: SessionRef,
    policy_revision: PolicyRevision,
    catalog_digest: Sha256Digest,
    issued_at: Timestamp,
    expires_at: Timestamp,
    grants: Vec<CapabilityGrant>,
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
        let digest = grant_digest(&parts).map_err(|message| vec![invalid(message)])?;
        Ok(Self {
            admission_id: parts.admission_id,
            actor: parts.actor,
            agent: parts.agent,
            task: parts.task,
            session: parts.session,
            policy_revision: parts.policy_revision,
            catalog_digest: parts.catalog_digest,
            issued_at: parts.issued_at,
            expires_at: parts.expires_at,
            grants: parts.grants,
            grant_digest: digest,
        })
    }

    pub fn admission_id(&self) -> &AdmissionId {
        &self.admission_id
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
    pub fn catalog_digest(&self) -> &Sha256Digest {
        &self.catalog_digest
    }
    pub fn issued_at(&self) -> Timestamp {
        self.issued_at
    }
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
    pub fn grants(&self) -> &[CapabilityGrant] {
        &self.grants
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
            actor: ActorRef,
            agent: AgentRef,
            task: TaskRef,
            session: SessionRef,
            policy_revision: PolicyRevision,
            catalog_digest: Sha256Digest,
            issued_at: Timestamp,
            expires_at: Timestamp,
            grants: Vec<CapabilityGrant>,
            grant_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(deserializer)?;
        let value = Self::try_new(CapabilityGrantSetParts {
            admission_id: raw.admission_id,
            actor: raw.actor,
            agent: raw.agent,
            task: raw.task,
            session: raw.session,
            policy_revision: raw.policy_revision,
            catalog_digest: raw.catalog_digest,
            issued_at: raw.issued_at,
            expires_at: raw.expires_at,
            grants: raw.grants,
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
        actor: &'a ActorRef,
        agent: &'a AgentRef,
        task: &'a TaskRef,
        session: &'a SessionRef,
        policy_revision: &'a PolicyRevision,
        catalog_digest: &'a Sha256Digest,
        issued_at: Timestamp,
        expires_at: Timestamp,
        grants: &'a [CapabilityGrant],
    }
    canonical_digest(
        b"kiteframe:capability-grant-set:v1\0",
        &Wire {
            admission_id: &parts.admission_id,
            actor: &parts.actor,
            agent: &parts.agent,
            task: &parts.task,
            session: &parts.session,
            policy_revision: &parts.policy_revision,
            catalog_digest: &parts.catalog_digest,
            issued_at: parts.issued_at,
            expires_at: parts.expires_at,
            grants: &parts.grants,
        },
    )
}

fn canonical_digest<T: Serialize>(domain: &[u8], value: &T) -> Result<Sha256Digest, String> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| "value cannot be canonicalized".to_owned())?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(canonical);
    Ok(Sha256Digest::from_bytes(hasher.finalize().into()))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationRequest {
    invocation_id: InvocationId,
    admission_id: AdmissionId,
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

    pub fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }
    pub fn admission_id(&self) -> &AdmissionId {
        &self.admission_id
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StableCapabilityError {
    code: String,
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Suspension {
    checkpoint_ref: String,
}

impl Suspension {
    pub fn try_new(checkpoint_ref: impl Into<String>) -> Result<Self, String> {
        let checkpoint_ref = checkpoint_ref.into();
        if checkpoint_ref.trim().is_empty() {
            return Err("suspension checkpoint reference is required".to_owned());
        }
        Ok(Self { checkpoint_ref })
    }

    pub fn checkpoint_ref(&self) -> &str {
        &self.checkpoint_ref
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
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
        diagnostic: Diagnostic,
    },
}

impl InvocationOutcome {
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Self::Denied { diagnostic, .. } | Self::OutcomeUnknown { diagnostic, .. } => {
                Some(diagnostic)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
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
        diagnostic: Diagnostic,
    },
}

fn invalid(message: impl Into<SafeMessage>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Validate,
        message,
    )
}
