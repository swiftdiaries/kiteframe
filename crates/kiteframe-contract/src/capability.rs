use std::{collections::BTreeSet, fmt, num::NonZeroU64};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    CapabilityName, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, RetryClass,
    SafeMessage, Sha256Digest,
};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct CapabilityReleaseVersion(String);

impl CapabilityReleaseVersion {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        Version::parse(&value).map_err(|_| "invalid capability release version".to_owned())?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for CapabilityReleaseVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
impl fmt::Display for CapabilityReleaseVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityIdentity {
    name: CapabilityName,
    version: CapabilityReleaseVersion,
}
impl CapabilityIdentity {
    pub fn try_new(
        name: CapabilityName,
        version: CapabilityReleaseVersion,
    ) -> Result<Self, String> {
        Ok(Self { name, version })
    }
    pub fn name(&self) -> &CapabilityName {
        &self.name
    }
    pub fn version(&self) -> &CapabilityReleaseVersion {
        &self.version
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct JsonSchema2020_12(Value);
impl JsonSchema2020_12 {
    pub fn try_new(value: Value) -> Result<Self, String> {
        validate_schema(&value).map(|()| Self(value))
    }
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}
impl JsonSchema for JsonSchema2020_12 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonSchema2020_12".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::JsonSchema2020_12").into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "object" })
    }
}
impl<'de> Deserialize<'de> for JsonSchema2020_12 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_new(Value::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

fn validate_schema(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "JSON Schema must be an object".to_owned())?;
    if let Some(schema) = object.get("$schema")
        && schema != "https://json-schema.org/draft/2020-12/schema"
    {
        return Err("schema must use JSON Schema 2020-12".to_owned());
    }
    if !jsonschema::draft202012::meta::is_valid(value) {
        return Err("schema is not valid Draft 2020-12 JSON Schema".to_owned());
    }

    fn visit(value: &Value) -> Result<(), String> {
        match value {
            Value::Object(map) => {
                for (keyword, reference) in map {
                    if matches!(keyword.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef") {
                        let Value::String(reference) = reference else {
                            return Err("schema reference must be a string".to_owned());
                        };
                        if !reference.starts_with('#') {
                            return Err("remote schema reference is forbidden".to_owned());
                        }
                    }
                }
                for value in map.values() {
                    visit(value)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(value).and_then(|()| {
        jsonschema::draft202012::options()
            .build(value)
            .map(|_| ())
            .map_err(|_| "schema references must resolve from its bundled definitions".to_owned())
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ResourceSelectorSchema(JsonSchema2020_12);
impl ResourceSelectorSchema {
    pub fn try_new(value: Value) -> Result<Self, String> {
        JsonSchema2020_12::try_new(value).map(Self)
    }
    pub fn as_schema(&self) -> &JsonSchema2020_12 {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct NonEmptySet<T: Ord>(#[schemars(length(min = 1))] BTreeSet<T>);
impl<T: Ord> NonEmptySet<T> {
    pub fn try_new(values: BTreeSet<T>) -> Result<Self, String> {
        if values.is_empty() {
            Err("set must not be empty".to_owned())
        } else {
            Ok(Self(values))
        }
    }
    pub fn as_set(&self) -> &BTreeSet<T> {
        &self.0
    }
}
impl<'de, T> Deserialize<'de> for NonEmptySet<T>
where
    T: Deserialize<'de> + Ord,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_new(BTreeSet::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Immediate,
    Deferred,
    Suspendable,
}
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EffectClassification {
    ReadOnly,
    ReversibleWrite,
    IrreversibleWrite,
    ExternalSideEffect,
}
impl EffectClassification {
    fn effectful(self) -> bool {
        self != Self::ReadOnly
    }
}
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyScope {
    ActorCapabilityResourceOperation,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum IdempotencyRequirement {
    None,
    Required {
        scope: IdempotencyScope,
        retention_seconds: NonZeroU64,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreshnessRequirement {
    #[serde(default)]
    pub max_admission_age_seconds: Option<NonZeroU64>,
    #[serde(default)]
    pub policy_revision_required: bool,
    #[serde(default)]
    pub max_input_age_seconds: Option<NonZeroU64>,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreconditionKind {
    EntityVersion,
    Etag,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreconditionDescriptor {
    pub name: String,
    pub kind: PreconditionKind,
    #[serde(default)]
    pub required: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ConfirmationRequirement {
    None,
    Required { evidence: EvidenceRequirement },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ApprovalRequirement {
    None,
    Required { evidence: EvidenceRequirement },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ConsentRequirement {
    None,
    Required { evidence: EvidenceRequirement },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRequirement {
    pub kind: String,
    #[serde(default)]
    pub issuer: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityErrorDescriptor {
    code: String,
    category: String,
    retry: RetryClass,
    message: SafeMessage,
}
impl CapabilityErrorDescriptor {
    pub fn try_new(
        code: impl Into<String>,
        category: impl Into<String>,
        retry: impl AsRef<str>,
        message: impl Into<SafeMessage>,
    ) -> Result<Self, String> {
        let retry = match retry.as_ref() {
            "never" => RetryClass::Never,
            "after_refresh" => RetryClass::AfterRefresh,
            "after_user_action" => RetryClass::AfterUserAction,
            "status_first" => RetryClass::StatusFirst,
            _ => return Err("invalid stable error retry class".to_owned()),
        };
        let code = code.into();
        let category = category.into();
        if code.is_empty() || category.is_empty() {
            return Err("stable error code and category are required".to_owned());
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
}

#[derive(Clone, Debug)]
pub struct CapabilityDescriptorParts {
    pub identity: CapabilityIdentity,
    pub summary: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub stable_errors: Vec<CapabilityErrorDescriptor>,
    pub execution_modes: NonEmptySet<ExecutionMode>,
    pub resource_selector_schema: ResourceSelectorSchema,
    pub effect: EffectClassification,
    pub idempotency: IdempotencyRequirement,
    pub freshness: FreshnessRequirement,
    pub preconditions: Vec<PreconditionDescriptor>,
    pub confirmation: ConfirmationRequirement,
    pub approval: ApprovalRequirement,
    pub consent: ConsentRequirement,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityDescriptor {
    identity: CapabilityIdentity,
    summary: String,
    input_schema: JsonSchema2020_12,
    output_schema: JsonSchema2020_12,
    stable_errors: Vec<CapabilityErrorDescriptor>,
    execution_modes: NonEmptySet<ExecutionMode>,
    resource_selector_schema: ResourceSelectorSchema,
    effect: EffectClassification,
    idempotency: IdempotencyRequirement,
    freshness: FreshnessRequirement,
    preconditions: Vec<PreconditionDescriptor>,
    confirmation: ConfirmationRequirement,
    approval: ApprovalRequirement,
    consent: ConsentRequirement,
    descriptor_digest: Sha256Digest,
}
impl CapabilityDescriptor {
    pub fn try_new(parts: CapabilityDescriptorParts) -> Result<Self, Vec<Diagnostic>> {
        let mut errors = Vec::new();
        if parts.summary.trim().is_empty() {
            errors.push(invalid("capability summary is required"));
        }
        let mut stable_error_codes = BTreeSet::new();
        if parts
            .stable_errors
            .iter()
            .any(|error| !stable_error_codes.insert(error.code()))
        {
            errors.push(invalid("stable error machine codes must be unique"));
        }
        if parts.effect.effectful() && matches!(parts.idempotency, IdempotencyRequirement::None) {
            errors.push(invalid("effectful capability requires idempotency"));
        }
        let input_schema = match JsonSchema2020_12::try_new(parts.input_schema) {
            Ok(value) => value,
            Err(message) => {
                errors.push(invalid(message));
                return Err(errors);
            }
        };
        let output_schema = match JsonSchema2020_12::try_new(parts.output_schema) {
            Ok(value) => value,
            Err(message) => {
                errors.push(invalid(message));
                return Err(errors);
            }
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut stable_errors = parts.stable_errors;
        stable_errors.sort();
        stable_errors.dedup();
        let mut preconditions = parts.preconditions;
        preconditions.sort();
        preconditions.dedup();
        let wire = DescriptorWire {
            identity: parts.identity,
            summary: parts.summary,
            input_schema,
            output_schema,
            stable_errors,
            execution_modes: parts.execution_modes,
            resource_selector_schema: parts.resource_selector_schema,
            effect: parts.effect,
            idempotency: parts.idempotency,
            freshness: parts.freshness,
            preconditions,
            confirmation: parts.confirmation,
            approval: parts.approval,
            consent: parts.consent,
        };
        let descriptor_digest = digest(&wire).map_err(|message| vec![invalid(message)])?;
        Ok(Self {
            identity: wire.identity,
            summary: wire.summary,
            input_schema: wire.input_schema,
            output_schema: wire.output_schema,
            stable_errors: wire.stable_errors,
            execution_modes: wire.execution_modes,
            resource_selector_schema: wire.resource_selector_schema,
            effect: wire.effect,
            idempotency: wire.idempotency,
            freshness: wire.freshness,
            preconditions: wire.preconditions,
            confirmation: wire.confirmation,
            approval: wire.approval,
            consent: wire.consent,
            descriptor_digest,
        })
    }
    pub fn identity(&self) -> &CapabilityIdentity {
        &self.identity
    }
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn descriptor_digest(&self) -> &Sha256Digest {
        &self.descriptor_digest
    }
    pub fn input_schema(&self) -> &JsonSchema2020_12 {
        &self.input_schema
    }
    pub fn output_schema(&self) -> &JsonSchema2020_12 {
        &self.output_schema
    }
    pub fn stable_errors(&self) -> &[CapabilityErrorDescriptor] {
        &self.stable_errors
    }
    pub fn execution_modes(&self) -> &NonEmptySet<ExecutionMode> {
        &self.execution_modes
    }
    pub fn resource_selector_schema(&self) -> &ResourceSelectorSchema {
        &self.resource_selector_schema
    }
    pub fn effect(&self) -> EffectClassification {
        self.effect
    }
    pub fn freshness(&self) -> &FreshnessRequirement {
        &self.freshness
    }
    pub fn preconditions(&self) -> &[PreconditionDescriptor] {
        &self.preconditions
    }
    pub fn confirmation(&self) -> &ConfirmationRequirement {
        &self.confirmation
    }
    pub fn approval(&self) -> &ApprovalRequirement {
        &self.approval
    }
    pub fn idempotency(&self) -> &IdempotencyRequirement {
        &self.idempotency
    }
    pub fn consent(&self) -> &ConsentRequirement {
        &self.consent
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DescriptorWire {
    identity: CapabilityIdentity,
    summary: String,
    input_schema: JsonSchema2020_12,
    output_schema: JsonSchema2020_12,
    stable_errors: Vec<CapabilityErrorDescriptor>,
    execution_modes: NonEmptySet<ExecutionMode>,
    resource_selector_schema: ResourceSelectorSchema,
    effect: EffectClassification,
    idempotency: IdempotencyRequirement,
    freshness: FreshnessRequirement,
    preconditions: Vec<PreconditionDescriptor>,
    confirmation: ConfirmationRequirement,
    approval: ApprovalRequirement,
    consent: ConsentRequirement,
}
impl<'de> Deserialize<'de> for CapabilityDescriptor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            identity: CapabilityIdentity,
            summary: String,
            input_schema: Value,
            output_schema: Value,
            stable_errors: Vec<CapabilityErrorDescriptor>,
            execution_modes: NonEmptySet<ExecutionMode>,
            resource_selector_schema: ResourceSelectorSchema,
            effect: EffectClassification,
            idempotency: IdempotencyRequirement,
            freshness: FreshnessRequirement,
            preconditions: Vec<PreconditionDescriptor>,
            confirmation: ConfirmationRequirement,
            approval: ApprovalRequirement,
            consent: ConsentRequirement,
            descriptor_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(deserializer)?;
        let descriptor = Self::try_new(CapabilityDescriptorParts {
            identity: raw.identity,
            summary: raw.summary,
            input_schema: raw.input_schema,
            output_schema: raw.output_schema,
            stable_errors: raw.stable_errors,
            execution_modes: raw.execution_modes,
            resource_selector_schema: raw.resource_selector_schema,
            effect: raw.effect,
            idempotency: raw.idempotency,
            freshness: raw.freshness,
            preconditions: raw.preconditions,
            confirmation: raw.confirmation,
            approval: raw.approval,
            consent: raw.consent,
        })
        .map_err(|errors| D::Error::custom(errors[0].message.as_str()))?;
        if descriptor.descriptor_digest != raw.descriptor_digest {
            return Err(D::Error::custom(
                "descriptor digest does not match canonical descriptor",
            ));
        }
        Ok(descriptor)
    }
}
fn invalid(message: impl Into<SafeMessage>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PackageInvalid,
        DiagnosticCategory::Package,
        DiagnosticStage::Validate,
        message,
    )
}
pub(crate) fn digest<T: Serialize>(value: &T) -> Result<Sha256Digest, String> {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| "value cannot be canonicalized".to_owned())?;
    Ok(Sha256Digest::from_bytes(Sha256::digest(bytes).into()))
}
