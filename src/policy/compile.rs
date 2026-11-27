use std::fmt::Write;
use std::str::FromStr;

use crate::{Capability, ClosedSchema, PrimitiveValue, ToolCatalogSnapshot};

use super::{PolicyEffect, PolicyError, PolicyRule, PolicyTarget, Primitive, SessionPolicy};

pub(super) const RULE_ID_ANNOTATION: &str = "rule_id";
pub(super) const INPUT_CONTEXT_KEY: &str = "input";
pub(super) const UNCOMPARABLE_CONTEXT_KEY: &str = "uncomparable";

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
        let annotation = format!("@{RULE_ID_ANNOTATION}({})", cedar_string(&rule.rule_id));
        match &rule.target {
            PolicyTarget::Tool { names } if rule.effect == PolicyEffect::Forbid => {
                let set = names
                    .iter()
                    .map(|name| cedar_string(name))
                    .collect::<Vec<_>>()
                    .join(", ");
                writeln!(
                    &mut cedar,
                    "{annotation}\nforbid(principal, action, resource) when {{ [{set}].contains(context.tool){} }};",
                    input_clauses(rule).map(|clauses| format!(" && {clauses}")).unwrap_or_default()
                )
                .expect("write to string");
            }
            PolicyTarget::Tool { names } => {
                for name in names {
                    let tool = catalog
                        .tools()
                        .get(name)
                        .ok_or_else(|| PolicyError::InvisibleTool(name.clone()))?;
                    let identity = cedar_string(&format!(
                        "{}#{}",
                        tool.qualified_name(),
                        tool.descriptor_digest()
                    ));
                    writeln!(&mut cedar, "{annotation}\n{effect}(principal, action == Action::{identity}, resource == Kiteframe::Tool::{identity}){};", input_clauses(rule).map(|clauses| format!(" when {{ {clauses} }}")).unwrap_or_default()).expect("write to string");
                }
            }
            PolicyTarget::Capability { names } => {
                let set = names
                    .iter()
                    .map(|capability| cedar_string(capability_name(capability)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let condition = match rule.effect {
                    PolicyEffect::Forbid => format!("context.capabilities.containsAny([{set}])"),
                    PolicyEffect::Permit => format!(
                        "!context.capabilities.isEmpty() && [{set}].containsAll(context.capabilities)"
                    ),
                };
                writeln!(
                    &mut cedar,
                    "{annotation}\n{effect}(principal, action, resource) when {{ {condition} }};"
                )
                .expect("write to string");
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

fn input_clauses(rule: &PolicyRule) -> Option<String> {
    if rule.input.is_empty() {
        return None;
    }
    let clauses = rule
        .input
        .iter()
        .map(|(pointer, allowed)| {
            let key = cedar_string(pointer);
            let present = format!("context.{INPUT_CONTEXT_KEY} has {key}");
            let values = allowed
                .0
                .iter()
                .filter_map(|value| match value {
                    Primitive::String(value) => Some(cedar_string(value)),
                    Primitive::Integer(value) => Some(value.to_string()),
                    Primitive::Boolean(value) => Some(value.to_string()),
                    Primitive::Number(_) | Primitive::Null => None,
                })
                .map(|literal| format!("context.{INPUT_CONTEXT_KEY}[{key}] == {literal}"))
                .collect::<Vec<_>>();
            let listed = if values.is_empty() {
                "false".to_owned()
            } else {
                format!("({present} && ({}))", values.join(" || "))
            };
            match rule.effect {
                PolicyEffect::Forbid => format!("(!({present}) || {listed})"),
                PolicyEffect::Permit if allowed.0.contains(&Primitive::Null) => format!(
                    "((!({present}) && !context.{UNCOMPARABLE_CONTEXT_KEY}.contains({key})) || {listed})"
                ),
                PolicyEffect::Permit => listed,
            }
        })
        .collect::<Vec<_>>();
    Some(clauses.join(" && "))
}

pub(super) fn cedar_string(value: &str) -> String {
    format!("\"{}\"", value.escape_default())
}

pub(super) fn capability_name(capability: &Capability) -> &'static str {
    match capability {
        Capability::McpUnknown => "mcp_unknown",
        Capability::ReadData => "read_data",
        Capability::WriteData => "write_data",
        Capability::Network => "network",
        Capability::Filesystem => "filesystem",
    }
}
