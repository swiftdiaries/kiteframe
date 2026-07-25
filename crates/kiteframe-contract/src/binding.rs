use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{BindingSchemaVersion, DataClassification, ModelRole, RuntimeBindingKind};

fn is_symbol(value: &str) -> bool {
    let mut previous_separator = false;
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    for character in chars {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            previous_separator = false;
        } else if matches!(character, '.' | '_' | '-') && !previous_separator {
            previous_separator = true;
        } else {
            return false;
        }
    }
    !previous_separator
}

macro_rules! symbol_newtype {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
        #[serde(transparent)]
        #[schemars(transparent)]
        pub struct $name(
            #[schemars(regex(pattern = r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$"))] String,
        );

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if !is_symbol(&value) {
                    return Err(format!("invalid {}", stringify!($name)));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

symbol_newtype!(RuntimeTarget);
symbol_newtype!(RegistrySymbol);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeBinding {
    pub api_version: BindingSchemaVersion,
    pub kind: RuntimeBindingKind,
    pub metadata: RuntimeBindingMetadata,
    pub spec: RuntimeBindingSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeBindingMetadata {
    pub runtime: RuntimeTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeBindingSpec {
    pub models: BTreeMap<ModelRole, RegistrySymbol>,
    #[serde(default)]
    pub components: TypedComponentSymbols,
    pub capability_provider: RegistrySymbol,
    pub audit_sink: RegistrySymbol,
    #[serde(default)]
    pub content_capture: Option<BindingContentCapturePolicy>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedComponentSymbols {
    #[serde(default)]
    pub middleware: Vec<RegistrySymbol>,
    #[serde(default)]
    pub backend: Option<RegistrySymbol>,
    #[serde(default)]
    pub checkpointer: Option<RegistrySymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingContentCapturePolicy {
    pub enabled: bool,
    pub classifications: BTreeSet<DataClassification>,
    pub redaction_policy: RegistrySymbol,
    pub retention_policy: RegistrySymbol,
    pub access_policy: RegistrySymbol,
    pub encrypted_content_store: RegistrySymbol,
}
