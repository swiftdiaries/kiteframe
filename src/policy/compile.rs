use std::fmt::Write;
use std::str::FromStr;

use crate::{Capability, ClosedSchema, PrimitiveValue, ToolCatalogSnapshot};

use super::{PolicyEffect, PolicyError, PolicyTarget, SessionPolicy};

#[derive(Clone, Debug, serde::Serialize)]
pub struct CompiledPolicy {
    pub cedar: String,
    pub schema: String,
}

pub fn compile(
    policy: &SessionPolicy,
    catalog: &ToolCatalogSnapshot,
) -> Result<CompiledPolicy, PolicyError> {
    policy.validate(catalog)?;
    let schema = generated_schema(catalog)?;
    let mut cedar = String::new();
    for rule in &policy.rules {
        let effect = match rule.effect {
            PolicyEffect::Permit => "permit",
            PolicyEffect::Forbid => "forbid",
        };
        match &rule.target {
            PolicyTarget::Tool { names } => {
                for name in names {
                    let tool = catalog
                        .tools()
                        .get(name)
                        .ok_or_else(|| PolicyError::InvisibleTool(name.clone()))?;
                    let identity =
                        format!("{}#{}", tool.qualified_name(), tool.descriptor_digest());
                    writeln!(&mut cedar, "{effect}(principal, action == Action::\"{identity}\", resource == Kiteframe::Tool::\"{identity}\");").expect("write to string");
                }
            }
            PolicyTarget::Capability { names } => {
                for capability in names {
                    writeln!(
                        &mut cedar,
                        "{effect}(principal, action == Action::\"capability:{}\", resource);",
                        capability_name(capability)
                    )
                    .expect("write to string");
                }
            }
        }
    }
    cedar_policy::PolicySet::from_str(&cedar)
        .map_err(|error| PolicyError::Cedar(error.to_string()))?;
    Ok(CompiledPolicy { cedar, schema })
}

fn generated_schema(catalog: &ToolCatalogSnapshot) -> Result<String, PolicyError> {
    let tools = catalog.tools().values().map(|tool| serde_json::json!({"name": generator_action_name(tool.qualified_name(), tool.descriptor_digest()), "description": tool.description(), "inputSchema": json_schema(tool.input()), "outputSchema": json_schema(tool.output())})).collect::<Vec<_>>();
    let description = mcp_tools_sdk::description::ServerDescription::from_json_str(
        &serde_json::json!({"result":{"tools": tools}}).to_string(),
    )
    .map_err(|error| PolicyError::Cedar(error.to_string()))?;
    let mut generator = cedar_policy_mcp_schema_generator::SchemaGenerator::from_cedarschema_str("namespace Kiteframe { @mcp_principal(\"Session\") entity Session; @mcp_resource(\"Tool\") entity Tool; }").map_err(|error| PolicyError::Cedar(error.to_string()))?;
    generator
        .add_actions_from_server_description(&description)
        .map_err(|error| PolicyError::Cedar(error.to_string()))?;
    Ok(generator.get_schema_as_str())
}

fn generator_action_name(name: &str, digest: &str) -> String {
    let normalized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{normalized}_{}", &digest[..12])
}

fn json_schema(schema: &ClosedSchema) -> serde_json::Value {
    match schema {
        ClosedSchema::Boolean => serde_json::json!({"type":"boolean"}),
        ClosedSchema::Integer => serde_json::json!({"type":"integer"}),
        ClosedSchema::Number => serde_json::json!({"type":"number"}),
        ClosedSchema::String => serde_json::json!({"type":"string"}),
        ClosedSchema::Array(items) => {
            serde_json::json!({"type":"array","items":json_schema(items)})
        }
        ClosedSchema::Object { properties, .. } => {
            serde_json::json!({"type":"object","properties": properties.iter().map(|(name, schema)| (name.clone(), json_schema(schema))).collect::<serde_json::Map<_, _>>(),"required":properties.keys().collect::<Vec<_>>()})
        }
        ClosedSchema::Enum(values) => {
            serde_json::json!({"enum": values.iter().map(primitive_json).collect::<Vec<_>>() })
        }
        ClosedSchema::Nullable(inner) => {
            serde_json::json!({"anyOf":[json_schema(inner),{"type":"null"}]})
        }
    }
}
fn primitive_json(value: &PrimitiveValue) -> serde_json::Value {
    match value {
        PrimitiveValue::Boolean(value) => serde_json::json!(value),
        PrimitiveValue::Integer(value) => serde_json::json!(value),
        PrimitiveValue::String(value) => serde_json::json!(value),
    }
}

fn capability_name(capability: &Capability) -> &'static str {
    match capability {
        Capability::McpUnknown => "mcp_unknown",
        Capability::ReadData => "read_data",
        Capability::WriteData => "write_data",
        Capability::Network => "network",
        Capability::Filesystem => "filesystem",
    }
}
