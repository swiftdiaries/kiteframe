use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::{CapabilityDescriptor, Sha256Digest, Timestamp, capability::digest};

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
    issued_at: Timestamp,
    expires_at: Option<Timestamp>,
    descriptors: Vec<CapabilityDescriptor>,
    catalog_digest: Sha256Digest,
}
impl CapabilityCatalog {
    pub fn try_new(
        identity: CatalogIdentity,
        issued_at: Timestamp,
        expires_at: Option<Timestamp>,
        mut descriptors: Vec<CapabilityDescriptor>,
    ) -> Result<Self, String> {
        if expires_at.is_some_and(|expires_at| expires_at <= issued_at) {
            return Err("catalog expiry must be after its issue time".to_owned());
        }
        descriptors.sort_by(|a, b| a.identity().cmp(b.identity()));
        if descriptors
            .windows(2)
            .any(|pair| pair[0].identity() == pair[1].identity())
        {
            return Err("catalog contains duplicate capability identities".to_owned());
        }
        let catalog_digest = digest(&CatalogWire {
            identity: identity.clone(),
            issued_at,
            expires_at,
            descriptors: &descriptors,
        })?;
        Ok(Self {
            identity,
            issued_at,
            expires_at,
            descriptors,
            catalog_digest,
        })
    }
    pub fn identity(&self) -> &CatalogIdentity {
        &self.identity
    }
    pub const fn issued_at(&self) -> Timestamp {
        self.issued_at
    }
    pub const fn expires_at(&self) -> Option<Timestamp> {
        self.expires_at
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
    issued_at: Timestamp,
    expires_at: Option<Timestamp>,
    descriptors: &'a [CapabilityDescriptor],
}
impl<'de> Deserialize<'de> for CapabilityCatalog {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            identity: CatalogIdentity,
            issued_at: Timestamp,
            #[serde(default)]
            expires_at: Option<Timestamp>,
            descriptors: Vec<CapabilityDescriptor>,
            catalog_digest: Sha256Digest,
        }
        let raw = Raw::deserialize(d)?;
        let catalog = Self::try_new(raw.identity, raw.issued_at, raw.expires_at, raw.descriptors)
            .map_err(serde::de::Error::custom)?;
        if catalog.catalog_digest != raw.catalog_digest {
            return Err(serde::de::Error::custom(
                "catalog digest does not match canonical catalog",
            ));
        }
        Ok(catalog)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", deny_unknown_fields)]
pub enum CatalogFetchResult {
    Modified { catalog: CapabilityCatalog },
    NotModified { catalog_digest: Sha256Digest },
}

impl CatalogFetchResult {
    pub fn not_modified(catalog_digest: Sha256Digest) -> Self {
        Self::NotModified { catalog_digest }
    }
}

impl JsonSchema for CatalogFetchResult {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CatalogFetchResult".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::CatalogFetchResult").into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "catalog": { "$ref": "capability-catalog.schema.json" },
                        "status": { "type": "string", "const": "modified" }
                    },
                    "additionalProperties": false,
                    "required": ["status", "catalog"]
                },
                {
                    "type": "object",
                    "properties": {
                        "catalog_digest": { "$ref": "#/$defs/Sha256Digest" },
                        "status": { "type": "string", "const": "not_modified" }
                    },
                    "additionalProperties": false,
                    "required": ["status", "catalog_digest"]
                }
            ],
            "$defs": {
                "Sha256Digest": {
                    "type": "string",
                    "pattern": "^[0-9a-f]{64}$"
                }
            }
        })
    }
}
