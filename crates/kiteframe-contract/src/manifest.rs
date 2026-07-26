use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::NonZeroU32,
};

use schemars::JsonSchema;
use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::{AgentKind, AgentSchemaVersion, PackagePath};

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

fn is_package_version(value: &str) -> bool {
    Version::parse(value).is_ok()
}

fn is_capability_version(value: &str) -> bool {
    let Some(version) = value.strip_prefix('^') else {
        return false;
    };
    let components: Vec<_> = version.split('.').collect();
    (components.len() == 2 || components.len() == 3)
        && components.iter().all(|component| {
            !component.is_empty()
                && component
                    .chars()
                    .all(|character| character.is_ascii_digit())
                && (*component == "0" || !component.starts_with('0'))
        })
        && VersionReq::parse(value).is_ok()
}

fn is_feature(value: &str) -> bool {
    let Some((name, major)) = value.rsplit_once('@') else {
        return false;
    };
    is_symbol(name)
        && !major.starts_with('0')
        && !major.is_empty()
        && major.chars().all(|character| character.is_ascii_digit())
}

fn is_resource_selector(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

macro_rules! validated_string {
    ($name:ident, $validator:ident, $pattern:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
        #[serde(transparent)]
        #[schemars(transparent)]
        pub struct $name(#[schemars(regex(pattern = $pattern))] String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if !$validator(&value) {
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

macro_rules! validated_dynamic_string {
    ($name:ident, $validator:ident, $transform:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
        #[serde(transparent)]
        #[schemars(transparent)]
        pub struct $name(#[schemars(transform = $transform)] String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if !$validator(&value) {
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

fn bounded_u64_decimal_pattern() -> String {
    const MAX: &str = "18446744073709551615";

    let mut alternatives = vec!["0".to_owned(), "[1-9][0-9]{0,18}".to_owned()];
    for (index, byte) in MAX.bytes().enumerate().skip(1) {
        let digit = byte - b'0';
        if digit == 0 {
            continue;
        }
        let prefix = &MAX[..index];
        let suffix_digits = MAX.len() - index - 1;
        let suffix = if suffix_digits == 0 {
            String::new()
        } else {
            format!("[0-9]{{{suffix_digits}}}")
        };
        alternatives.push(format!("{prefix}[0-{}]{suffix}", digit - 1));
    }
    alternatives.push(MAX.to_owned());
    format!("(?:{})", alternatives.join("|"))
}

fn package_version_pattern() -> String {
    let component = bounded_u64_decimal_pattern();
    format!(
        r"^{component}\.{component}\.{component}(?:-(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
    )
}

fn capability_version_pattern() -> String {
    let component = bounded_u64_decimal_pattern();
    format!(r"^\^{component}\.{component}(?:\.{component})?$")
}

fn set_package_version_pattern(schema: &mut schemars::Schema) {
    schema.insert("pattern".to_owned(), package_version_pattern().into());
}

fn set_capability_version_pattern(schema: &mut schemars::Schema) {
    schema.insert("pattern".to_owned(), capability_version_pattern().into());
}

validated_string!(AgentName, is_symbol, r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$");
validated_dynamic_string!(
    PackageVersion,
    is_package_version,
    set_package_version_pattern
);
validated_string!(ModelRole, is_symbol, r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$");
validated_string!(
    CapabilityName,
    is_symbol,
    r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$"
);
validated_dynamic_string!(
    CapabilityVersion,
    is_capability_version,
    set_capability_version_pattern
);
validated_string!(
    ResourceSelector,
    is_resource_selector,
    r"^[^\x00-\x1F\x7F-\x9F]+$"
);
validated_string!(
    Feature,
    is_feature,
    r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*@[1-9][0-9]*$"
);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentManifest {
    pub api_version: AgentSchemaVersion,
    pub kind: AgentKind,
    pub metadata: PackageIdentity,
    pub spec: AgentSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageIdentity {
    pub name: AgentName,
    pub version: PackageVersion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSpec {
    pub prompt: PromptRequirement,
    #[serde(default)]
    pub skills: Vec<PackagePath>,
    pub models: BTreeMap<ModelRole, ModelRequirement>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityRequirement>,
    #[serde(default)]
    pub delegation: Vec<DelegationRequirement>,
    #[serde(default)]
    pub features: FeatureRequirements,
    #[serde(default)]
    pub observability: ObservabilityRequirements,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptRequirement {
    pub system: PackagePath,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ModelCapability {
    Text,
    ToolCalling,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum LatencyClass {
    Interactive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRequirement {
    pub capabilities: BTreeSet<ModelCapability>,
    #[serde(default)]
    #[schemars(range(min = 1_u32, max = 4_294_967_295_u32))]
    pub min_context_tokens: Option<NonZeroU32>,
    #[serde(default)]
    pub max_latency_class: Option<LatencyClass>,
    #[serde(default = "default_true")]
    #[schemars(default = "default_true")]
    pub required: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityRequirement {
    pub name: CapabilityName,
    pub version: CapabilityVersion,
    #[serde(default = "default_true")]
    #[schemars(default = "default_true")]
    pub required: bool,
    #[serde(default)]
    pub resources: BTreeSet<ResourceSelector>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationRequirement {
    pub agent: PackagePath,
    #[serde(default)]
    pub capabilities: BTreeSet<CapabilityName>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureRequirements {
    #[serde(default)]
    pub required: BTreeSet<Feature>,
    #[serde(default)]
    pub optional: BTreeSet<Feature>,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DataClassification {
    Confidential,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservabilityRequirements {
    #[serde(default)]
    pub content_capture: ContentCaptureRequirement,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentCaptureRequirement {
    #[serde(default)]
    pub allowed: bool,
    #[serde(default)]
    pub classifications: BTreeSet<DataClassification>,
}
