use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{CapabilityDescriptor, CapabilityIdentity, FeatureSet, Sha256Digest};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockedCapability {
    identity: CapabilityIdentity,
    descriptor: CapabilityDescriptor,
    descriptor_digest: Sha256Digest,
    input_schema_digest: Sha256Digest,
    output_schema_digest: Sha256Digest,
    stable_error_set_digest: Sha256Digest,
    safety_metadata_digest: Sha256Digest,
}
impl LockedCapability {
    pub fn try_new(
        identity: CapabilityIdentity,
        descriptor: CapabilityDescriptor,
        descriptor_digest: Sha256Digest,
        input_schema_digest: Sha256Digest,
        output_schema_digest: Sha256Digest,
        stable_error_set_digest: Sha256Digest,
        safety_metadata_digest: Sha256Digest,
    ) -> Result<Self, String> {
        if identity != *descriptor.identity()
            || descriptor_digest != *descriptor.descriptor_digest()
        {
            return Err(
                "locked capability descriptor identity or digest does not match".to_owned(),
            );
        }
        Ok(Self {
            identity,
            descriptor,
            descriptor_digest,
            input_schema_digest,
            output_schema_digest,
            stable_error_set_digest,
            safety_metadata_digest,
        })
    }
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
impl<'de> Deserialize<'de> for LockedCapability {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            identity: CapabilityIdentity,
            descriptor: CapabilityDescriptor,
            descriptor_digest: Sha256Digest,
            input_schema_digest: Sha256Digest,
            output_schema_digest: Sha256Digest,
            stable_error_set_digest: Sha256Digest,
            safety_metadata_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(d)?;
        Self::try_new(
            raw.identity,
            raw.descriptor,
            raw.descriptor_digest,
            raw.input_schema_digest,
            raw.output_schema_digest,
            raw.stable_error_set_digest,
            raw.safety_metadata_digest,
        )
        .map_err(D::Error::custom)
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
