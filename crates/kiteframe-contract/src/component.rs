use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentMetadata {
    pub kind: ComponentKind,
    #[serde(default)]
    pub modalities: BTreeSet<String>,
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
