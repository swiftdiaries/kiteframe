use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CapabilityDescriptor, CapabilityIdentity, FeatureSet, Sha256Digest};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockedCapability {
    pub identity: CapabilityIdentity,
    pub descriptor: CapabilityDescriptor,
    pub descriptor_digest: Sha256Digest,
    pub input_schema_digest: Sha256Digest,
    pub output_schema_digest: Sha256Digest,
    pub stable_error_set_digest: Sha256Digest,
    pub safety_metadata_digest: Sha256Digest,
}
impl LockedCapability {
    pub fn identity(&self) -> &CapabilityIdentity {
        &self.identity
    }
    pub fn descriptor(&self) -> &CapabilityDescriptor {
        &self.descriptor
    }
    pub fn descriptor_digest(&self) -> &Sha256Digest {
        &self.descriptor_digest
    }
    pub fn input_schema_digest(&self) -> &Sha256Digest {
        &self.input_schema_digest
    }
    pub fn output_schema_digest(&self) -> &Sha256Digest {
        &self.output_schema_digest
    }
    pub fn stable_error_set_digest(&self) -> &Sha256Digest {
        &self.stable_error_set_digest
    }
    pub fn safety_metadata_digest(&self) -> &Sha256Digest {
        &self.safety_metadata_digest
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityLock {
    pub schema_version: LockSchemaVersion,
    pub package_portable_digest: Sha256Digest,
    pub catalog_identity: String,
    pub catalog_digest: Sha256Digest,
    pub catalog_revision: String,
    pub resolver_version: String,
    pub resolved_features: FeatureSet,
    pub capabilities: Vec<LockedCapability>,
    pub lock_digest: Sha256Digest,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum LockSchemaVersion {
    #[serde(rename = "kiteframe.dev/lock/v1alpha1")]
    V1Alpha1,
    #[serde(rename = "kiteframe.dev/lock/unsupported")]
    #[schemars(skip)]
    Unsupported,
}
