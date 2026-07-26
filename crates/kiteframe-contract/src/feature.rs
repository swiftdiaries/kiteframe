use std::{collections::BTreeSet, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct FeatureId(String);
impl FeatureId {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let Some((name, major)) = value.rsplit_once('@') else {
            return Err("feature must include a major version".to_owned());
        };
        if name.is_empty()
            || major.is_empty()
            || major.starts_with('0')
            || !major.chars().all(|c| c.is_ascii_digit())
        {
            return Err("invalid feature identifier".to_owned());
        }
        if major.parse::<u64>().is_err() {
            return Err("feature major version exceeds u64".to_owned());
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn name(&self) -> &str {
        self.0.rsplit_once('@').expect("validated feature").0
    }
    pub fn major(&self) -> u64 {
        self.0
            .rsplit_once('@')
            .expect("validated feature")
            .1
            .parse()
            .expect("validated feature")
    }
}
impl<'de> Deserialize<'de> for FeatureId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(D::Error::custom)
    }
}
impl fmt::Display for FeatureId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
pub type FeatureSet = BTreeSet<FeatureId>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureNegotiation {
    pub enabled_optional: FeatureSet,
    pub omitted_optional: FeatureSet,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilationReport {
    #[serde(default)]
    pub warnings: Vec<CompilationWarning>,
    #[serde(default)]
    pub decisions: Vec<CompilationDecision>,
}
impl CompilationReport {
    pub fn normalized(mut self) -> Self {
        self.warnings.sort();
        self.warnings.dedup();
        self.decisions.sort();
        self.decisions.dedup();
        self
    }
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilationWarning {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompilationDecision {
    pub subject: String,
    pub outcome: String,
}
