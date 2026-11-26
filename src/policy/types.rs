use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Capability, ClosedSchema, PrimitiveValue, ToolCatalogSnapshot};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionPolicy {
    pub version: u32,
    pub rules: Vec<PolicyRule>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyRule {
    pub rule_id: String,
    pub effect: PolicyEffect,
    pub target: PolicyTarget,
    #[serde(default)]
    pub input: BTreeMap<String, OneOfPrimitive>,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    Permit,
    Forbid,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PolicyTarget {
    Capability { names: BTreeSet<Capability> },
    Tool { names: BTreeSet<String> },
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Primitive {
    String(String),
    Integer(i64),
    Number(serde_json::Number),
    Boolean(bool),
    Null,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OneOfPrimitive(pub Vec<Primitive>);
#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("unsupported policy version {0}")]
    Version(u32),
    #[error("rule id must not be empty")]
    EmptyRuleId,
    #[error("duplicate rule id: {0}")]
    DuplicateRuleId(String),
    #[error("target must not be empty")]
    EmptyTarget,
    #[error("policy references invisible tool: {0}")]
    InvisibleTool(String),
    #[error("policy input pointer is invalid: {0}")]
    InvalidPointer(String),
    #[error("policy input constraint must not be empty: {0}")]
    EmptyOneOf(String),
    #[error("policy input pointer is not supported by selected target: {0}")]
    UnsupportedPath(String),
    #[error("policy input constraint contains a value outside the selected tool schema: {0}")]
    InvalidConstraintValue(String),
    #[error("policy input constraint uses a non-integer number, which Cedar cannot compare: {0}")]
    UnsupportedNumber(String),
    #[error("generated Cedar policy is invalid: {0}")]
    Cedar(String),
}
impl SessionPolicy {
    pub fn validate(&self, catalog: &ToolCatalogSnapshot) -> Result<(), PolicyError> {
        if self.version != 1 {
            return Err(PolicyError::Version(self.version));
        }
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            if rule.rule_id.trim().is_empty() {
                return Err(PolicyError::EmptyRuleId);
            }
            if !ids.insert(&rule.rule_id) {
                return Err(PolicyError::DuplicateRuleId(rule.rule_id.clone()));
            }
            match &rule.target {
                PolicyTarget::Capability { names } if names.is_empty() => {
                    return Err(PolicyError::EmptyTarget);
                }
                PolicyTarget::Tool { names } if names.is_empty() => {
                    return Err(PolicyError::EmptyTarget);
                }
                PolicyTarget::Tool { names } => {
                    for name in names {
                        if !catalog.tools().contains_key(name) {
                            return Err(PolicyError::InvisibleTool(name.clone()));
                        }
                    }
                }
                PolicyTarget::Capability { .. } => {}
            }
            for (pointer, allowed) in &rule.input {
                if !pointer.starts_with('/') || pointer.contains("//") {
                    return Err(PolicyError::InvalidPointer(pointer.clone()));
                }
                if allowed.0.is_empty() {
                    return Err(PolicyError::EmptyOneOf(pointer.clone()));
                }
                if allowed
                    .0
                    .iter()
                    .any(|value| matches!(value, Primitive::Number(_)))
                {
                    return Err(PolicyError::UnsupportedNumber(pointer.clone()));
                }
                let PolicyTarget::Tool { names } = &rule.target else {
                    return Err(PolicyError::UnsupportedPath(pointer.clone()));
                };
                for name in names {
                    let tool = &catalog.tools()[name];
                    let schema = schema_at_pointer(tool.input(), pointer)
                        .ok_or_else(|| PolicyError::UnsupportedPath(pointer.clone()))?;
                    if allowed
                        .0
                        .iter()
                        .any(|value| !primitive_matches_schema(value, schema))
                    {
                        return Err(PolicyError::InvalidConstraintValue(pointer.clone()));
                    }
                }
            }
        }
        Ok(())
    }
}

fn schema_at_pointer<'a>(schema: &'a ClosedSchema, pointer: &str) -> Option<&'a ClosedSchema> {
    pointer
        .trim_start_matches('/')
        .split('/')
        .try_fold(schema, |schema, segment| match schema {
            ClosedSchema::Object { properties, .. } => {
                properties.get(&segment.replace("~1", "/").replace("~0", "~"))
            }
            _ => None,
        })
}

fn primitive_matches_schema(value: &Primitive, schema: &ClosedSchema) -> bool {
    match (value, schema) {
        (Primitive::String(_), ClosedSchema::String)
        | (Primitive::Integer(_), ClosedSchema::Integer)
        | (Primitive::Number(_), ClosedSchema::Number)
        | (Primitive::Boolean(_), ClosedSchema::Boolean) => true,
        (Primitive::Null, ClosedSchema::Nullable(_)) => true,
        (value, ClosedSchema::Nullable(inner)) => primitive_matches_schema(value, inner),
        (Primitive::String(value), ClosedSchema::Enum(values)) => {
            values.contains(&PrimitiveValue::String(value.clone()))
        }
        (Primitive::Integer(value), ClosedSchema::Enum(values)) => {
            values.contains(&PrimitiveValue::Integer(*value))
        }
        (Primitive::Boolean(value), ClosedSchema::Enum(values)) => {
            values.contains(&PrimitiveValue::Boolean(*value))
        }
        _ => false,
    }
}
