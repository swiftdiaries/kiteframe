use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const AGENT_API_VERSION: &str = "kiteframe.dev/v1alpha1";
pub const BINDING_API_VERSION: &str = "kiteframe.dev/binding/v1alpha1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum AgentSchemaVersion {
    #[serde(rename = "kiteframe.dev/v1alpha1")]
    V1Alpha1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum AgentKind {
    Agent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum BindingSchemaVersion {
    #[serde(rename = "kiteframe.dev/binding/v1alpha1")]
    V1Alpha1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum RuntimeBindingKind {
    RuntimeBinding,
}
