use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum PrimitiveValue {
    Boolean(bool),
    Integer(i64),
    String(String),
}

impl PrimitiveValue {
    fn matches(&self, value: &Value) -> bool {
        match self {
            Self::Boolean(expected) => value.as_bool() == Some(*expected),
            Self::Integer(expected) => value.as_i64() == Some(*expected),
            Self::String(expected) => value.as_str() == Some(expected),
        }
    }

    fn normalized_json(&self) -> Value {
        match self {
            Self::Boolean(value) => json!({"boolean": value}),
            Self::Integer(value) => json!({"integer": value}),
            Self::String(value) => json!({"string": value}),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ClosedSchema {
    Boolean,
    Integer,
    Number,
    String,
    Array(Box<ClosedSchema>),
    Object {
        name: String,
        properties: BTreeMap<String, ClosedSchema>,
    },
    Enum(BTreeSet<PrimitiveValue>),
    Nullable(Box<ClosedSchema>),
}

impl ClosedSchema {
    #[must_use]
    pub const fn boolean() -> Self {
        Self::Boolean
    }

    #[must_use]
    pub const fn integer() -> Self {
        Self::Integer
    }

    #[must_use]
    pub const fn number() -> Self {
        Self::Number
    }

    #[must_use]
    pub const fn string() -> Self {
        Self::String
    }

    #[must_use]
    pub fn array(items: Self) -> Self {
        Self::Array(Box::new(items))
    }

    #[must_use]
    pub fn object(name: impl Into<String>, properties: BTreeMap<String, Self>) -> Self {
        Self::Object {
            name: name.into(),
            properties,
        }
    }

    #[must_use]
    pub fn enumeration(values: BTreeSet<PrimitiveValue>) -> Self {
        Self::Enum(values)
    }

    #[must_use]
    pub fn nullable(inner: Self) -> Self {
        Self::Nullable(Box::new(inner))
    }

    pub fn validate(&self, value: &Value) -> Result<(), SchemaError> {
        self.validate_at(value, "$")
    }

    pub fn validate_definition(&self) -> Result<(), SchemaError> {
        match self {
            Self::Object { name, properties } => {
                if name.trim().is_empty() {
                    return Err(SchemaError::EmptyObjectName);
                }
                for (property, schema) in properties {
                    if property.is_empty() {
                        return Err(SchemaError::EmptyPropertyName);
                    }
                    schema.validate_definition()?;
                }
            }
            Self::Array(items) | Self::Nullable(items) => items.validate_definition()?,
            Self::Enum(values) if values.is_empty() => return Err(SchemaError::EmptyEnum),
            _ => {}
        }
        Ok(())
    }

    #[must_use]
    pub fn normalized_json(&self) -> Value {
        match self {
            Self::Boolean => json!({"type": "boolean"}),
            Self::Integer => json!({"type": "integer"}),
            Self::Number => json!({"type": "number"}),
            Self::String => json!({"type": "string"}),
            Self::Array(items) => json!({"type": "array", "items": items.normalized_json()}),
            Self::Object { name, properties } => json!({
                "type": "object",
                "name": name,
                "additionalProperties": false,
                "required": properties.keys().collect::<Vec<_>>(),
                "properties": properties.iter().map(|(name, schema)| (name.clone(), schema.normalized_json())).collect::<serde_json::Map<_, _>>(),
            }),
            Self::Enum(values) => json!({
                "type": "enum",
                "values": values.iter().map(PrimitiveValue::normalized_json).collect::<Vec<_>>(),
            }),
            Self::Nullable(inner) => json!({"type": "nullable", "inner": inner.normalized_json()}),
        }
    }

    fn validate_at(&self, value: &Value, path: &str) -> Result<(), SchemaError> {
        match self {
            Self::Boolean if value.is_boolean() => Ok(()),
            Self::Integer if value.as_i64().is_some() || value.as_u64().is_some() => Ok(()),
            Self::Number if value.is_number() => Ok(()),
            Self::String if value.is_string() => Ok(()),
            Self::Array(items) => match value.as_array() {
                Some(values) => values.iter().enumerate().try_for_each(|(index, value)| {
                    items.validate_at(value, &format!("{path}[{index}]"))
                }),
                None => Err(SchemaError::WrongType {
                    path: path.to_owned(),
                    expected: "array",
                }),
            },
            Self::Object { properties, .. } => match value.as_object() {
                Some(values) => {
                    for property in properties.keys() {
                        if !values.contains_key(property) {
                            return Err(SchemaError::MissingProperty {
                                path: path.to_owned(),
                                property: property.clone(),
                            });
                        }
                    }
                    for (property, value) in values {
                        let Some(schema) = properties.get(property) else {
                            return Err(SchemaError::UnknownProperty {
                                path: path.to_owned(),
                                property: property.clone(),
                            });
                        };
                        schema.validate_at(value, &format!("{path}.{property}"))?;
                    }
                    Ok(())
                }
                None => Err(SchemaError::WrongType {
                    path: path.to_owned(),
                    expected: "object",
                }),
            },
            Self::Enum(values) if values.iter().any(|candidate| candidate.matches(value)) => Ok(()),
            Self::Enum(_) => Err(SchemaError::InvalidEnumValue {
                path: path.to_owned(),
            }),
            Self::Nullable(_) if value.is_null() => Ok(()),
            Self::Nullable(inner) => inner.validate_at(value, path),
            _ => Err(SchemaError::WrongType {
                path: path.to_owned(),
                expected: self.type_name(),
            }),
        }
    }

    const fn type_name(&self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
            Self::Array(_) => "array",
            Self::Object { .. } => "object",
            Self::Enum(_) => "enum member",
            Self::Nullable(_) => "nullable value",
        }
    }
}

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("{path}: missing required property `{property}`")]
    MissingProperty { path: String, property: String },
    #[error("{path}: unknown property `{property}`")]
    UnknownProperty { path: String, property: String },
    #[error("{path}: expected {expected}")]
    WrongType {
        path: String,
        expected: &'static str,
    },
    #[error("{path}: value is not an allowed primitive enum member")]
    InvalidEnumValue { path: String },
    #[error("closed object names must not be empty")]
    EmptyObjectName,
    #[error("closed object property names must not be empty")]
    EmptyPropertyName,
    #[error("primitive enums must contain at least one value")]
    EmptyEnum,
}
