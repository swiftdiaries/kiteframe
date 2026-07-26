use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::NonZeroU32,
};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{FeatureSet, RegistrySymbol, RuntimeTarget, Sha256Digest};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeTargetDescriptor {
    pub target: RuntimeTarget,
    pub supported_features: FeatureSet,
    pub target_digest: Sha256Digest,
}
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Model,
    Middleware,
    Backend,
    Checkpointer,
    CapabilityProvider,
    AuditSink,
    RedactionPolicy,
    RetentionPolicy,
    AccessPolicy,
    EncryptedContentStore,
}
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ModelModality {
    Text,
}
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ModelLatencyClass {
    Interactive,
    Batch,
}
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct ResidencyClass(
    #[schemars(regex(pattern = r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$"))] String,
);
impl ResidencyClass {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let mut previous_separator = false;
        let mut characters = value.chars();
        let valid = matches!(characters.next(), Some(first) if first.is_ascii_lowercase())
            && characters.all(|character| {
                if character.is_ascii_lowercase() || character.is_ascii_digit() {
                    previous_separator = false;
                    true
                } else if matches!(character, '.' | '_' | '-') && !previous_separator {
                    previous_separator = true;
                    true
                } else {
                    false
                }
            })
            && !previous_separator;
        if valid {
            Ok(Self(value))
        } else {
            Err("invalid residency class".to_owned())
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for ResidencyClass {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
impl fmt::Display for ResidencyClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelMetadata {
    pub modalities: BTreeSet<ModelModality>,
    pub tool_calling: bool,
    pub structured_output: bool,
    #[schemars(range(min = 1_u32, max = 4_294_967_295_u32))]
    pub max_context_tokens: NonZeroU32,
    pub residency: ResidencyClass,
    pub latency_class: ModelLatencyClass,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentMetadata {
    pub kind: ComponentKind,
    #[serde(default)]
    pub model: Option<ModelMetadata>,
    #[serde(default)]
    pub features: FeatureSet,
    #[serde(default)]
    pub durable: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentMetadataCatalog {
    pub target: RuntimeTarget,
    pub components: BTreeMap<RegistrySymbol, ComponentMetadata>,
}
