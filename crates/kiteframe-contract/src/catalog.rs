use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CapabilityDescriptor, Sha256Digest, capability::digest};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogIdentity {
    pub name: String,
    pub revision: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityCatalog {
    identity: CatalogIdentity,
    descriptors: Vec<CapabilityDescriptor>,
    catalog_digest: Sha256Digest,
}
impl CapabilityCatalog {
    pub fn try_new(
        identity: CatalogIdentity,
        mut descriptors: Vec<CapabilityDescriptor>,
    ) -> Result<Self, String> {
        descriptors.sort_by(|a, b| a.identity().cmp(b.identity()));
        if descriptors
            .windows(2)
            .any(|pair| pair[0].identity() == pair[1].identity())
        {
            return Err("catalog contains duplicate capability identities".to_owned());
        }
        let catalog_digest = digest(&CatalogWire {
            identity: identity.clone(),
            descriptors: &descriptors,
        })?;
        Ok(Self {
            identity,
            descriptors,
            catalog_digest,
        })
    }
    pub fn identity(&self) -> &CatalogIdentity {
        &self.identity
    }
    pub fn descriptors(&self) -> &[CapabilityDescriptor] {
        &self.descriptors
    }
    pub fn catalog_digest(&self) -> &Sha256Digest {
        &self.catalog_digest
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogWire<'a> {
    identity: CatalogIdentity,
    descriptors: &'a [CapabilityDescriptor],
}
impl<'de> Deserialize<'de> for CapabilityCatalog {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            identity: CatalogIdentity,
            descriptors: Vec<CapabilityDescriptor>,
            catalog_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(d)?;
        let catalog =
            Self::try_new(raw.identity, raw.descriptors).map_err(serde::de::Error::custom)?;
        if catalog.catalog_digest != raw.catalog_digest {
            return Err(serde::de::Error::custom(
                "catalog digest does not match canonical catalog",
            ));
        }
        Ok(catalog)
    }
}
