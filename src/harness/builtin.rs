use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::dsl::{CompiledTask, CompiledType};
use crate::{
    ClosedSchema, HarnessStructuralLimits, HarnessTool, ModelClient, ModelRequest, PrimitiveValue,
    RuntimeHistoryItem, RuntimeHistoryKind, StoreError,
};

use super::{
    HARNESS_PROTOCOL_VERSION, HarnessApprovalBroker, HarnessCancellation, HarnessCapabilities,
    HarnessEventSender, HarnessFailure, HarnessInvocationEvent, HarnessInvocationOutput,
    HarnessPlugin, HarnessProfile, HarnessRequestEnvelope, HarnessTerminal, emit_and_wait,
};

const DURABLE_GOAL_DIRECTIVE: &str = "Treat the complete structured goal in the first input item as durable across this invocation and every repeated or recovered invocation. Preserve its objective, criterion ids, human success statements, required evidence types, constraints, and blocked conditions. The complete_task function completes only this bounded harness task and yields invocation evidence; it never completes or blocks the enclosing Kiteframe goal. Only the Rust kernel's CompleteGoal and BlockGoal authority may complete or block that goal.";

#[derive(Clone, Debug, PartialEq)]
pub struct HarnessExecutionResult {
    pub outcome: String,
    pub fields: Value,
    pub history: Vec<RuntimeHistoryItem>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum HarnessExecutionError {
    #[error("harness command is unavailable: {0}")]
    Unavailable(&'static str),
    #[error("harness result is invalid: {0}")]
    InvalidResult(String),
    #[error("harness failed: {0}")]
    Failed(String),
}

pub struct BuiltinHarnessPlugin {
    profile: HarnessProfile,
    model: Arc<dyn ModelClient>,
}

impl BuiltinHarnessPlugin {
    #[must_use]
    pub fn new(profile: HarnessProfile, model: Arc<dyn ModelClient>) -> Self {
        Self { profile, model }
    }

    pub async fn execute_direct(
        &self,
        invocation: &crate::InvokeHarness,
    ) -> Result<HarnessExecutionResult, HarnessExecutionError> {
        validate_invocation_limits(&invocation.limits)?;
        let request = ModelRequest {
            instructions: format!("{}\n\n{}", invocation.prompt, DURABLE_GOAL_DIRECTIVE),
            input: std::iter::once(json!({
                "role": invocation.role,
                "goal": invocation.goal,
                "input": invocation.input,
            }))
            .chain(
                invocation
                    .projected_history
                    .iter()
                    .map(|item| item.payload.clone()),
            )
            .collect(),
            tools: std::iter::once(completion_tool_from_schema(&invocation.result_schema))
                .chain(invocation.tools.iter().map(harness_tool_json))
                .collect(),
        };
        let response = tokio::time::timeout(
            Duration::from_millis(invocation.limits.active_execution_millis_remaining),
            self.model.respond(request),
        )
        .await
        .map_err(|_| HarnessExecutionError::Failed("active_execution_limit_exceeded".to_owned()))?
        .map_err(|error| HarnessExecutionError::Failed(error.to_string()))?;

        if response.items.iter().any(|item| {
            serde_json::to_vec(item).map_or(true, |bytes| {
                bytes.len() as u64 > invocation.limits.response_item_bytes
            })
        }) {
            return Err(HarnessExecutionError::InvalidResult(
                "response_item_limit_exceeded".to_owned(),
            ));
        }
        let tool_calls = response
            .items
            .iter()
            .filter(|item| non_completion_tool_call(item))
            .count() as u64;
        if tool_calls > invocation.limits.tool_calls_per_response {
            return Err(HarnessExecutionError::InvalidResult(
                "tool_call_limit_exceeded".to_owned(),
            ));
        }
        let actions = response
            .items
            .iter()
            .filter(|item| is_function_call(item))
            .collect::<Vec<_>>();
        let [action] = actions.as_slice() else {
            return Err(HarnessExecutionError::InvalidResult(
                "model response must contain exactly one completion or tool call".to_owned(),
            ));
        };
        if non_completion_tool_call(action) {
            return Err(HarnessExecutionError::Unavailable("model_tool_calls"));
        }
        let (outcome, fields) = parse_completion_item(action).ok_or_else(|| {
            HarnessExecutionError::InvalidResult("completion item is malformed".to_owned())
        })?;
        validate_schema_completion(&invocation.result_schema, &outcome, &fields)?;
        let history = response
            .items
            .into_iter()
            .enumerate()
            .map(|(sequence, payload)| RuntimeHistoryItem {
                sequence: sequence as u64,
                kind: RuntimeHistoryKind::HarnessOutput,
                payload,
            })
            .collect();
        Ok(HarnessExecutionResult {
            outcome,
            fields,
            history,
        })
    }
}

#[async_trait::async_trait]
impl HarnessPlugin for BuiltinHarnessPlugin {
    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    async fn describe_capabilities(&self) -> Result<HarnessCapabilities, HarnessFailure> {
        Ok(HarnessCapabilities {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            implementation_name: "kiteframe-builtin".to_owned(),
            implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            supported: self.profile.required.clone(),
        })
    }

    async fn start_invocation(
        &self,
        request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        _approvals: Arc<dyn HarnessApprovalBroker>,
        cancellation: HarnessCancellation,
    ) -> HarnessTerminal {
        if let Err(error) = emit_and_wait(&events, HarnessInvocationEvent::Started).await {
            return HarnessTerminal::OutcomeUnknown(error);
        }
        tokio::select! {
            _ = cancellation.cancelled() => HarnessTerminal::Cancelled,
            result = self.execute_direct(&request.invocation.payload) => match result {
                Ok(result) => HarnessTerminal::Completed(HarnessInvocationOutput {
                    outcome: result.outcome,
                    fields: result.fields,
                    history: result.history,
                }),
                Err(HarnessExecutionError::Unavailable(detail)) => HarnessTerminal::Failed(
                    HarnessFailure::new("harness_capability_unsupported", detail),
                ),
                Err(error) => HarnessTerminal::Failed(
                    HarnessFailure::new("harness_failed", error.to_string()),
                ),
            },
        }
    }
}

fn validate_invocation_limits(
    limits: &HarnessStructuralLimits,
) -> Result<(), HarnessExecutionError> {
    if limits.model_turns_remaining == 0 || limits.active_execution_millis_remaining == 0 {
        return Err(HarnessExecutionError::Failed(
            "active_execution_limit_exceeded".to_owned(),
        ));
    }
    if limits.response_item_bytes == 0 {
        return Err(HarnessExecutionError::InvalidResult(
            "response_item_limit_exceeded".to_owned(),
        ));
    }
    Ok(())
}

fn non_completion_tool_call(item: &Value) -> bool {
    is_function_call(item) && item.get("name").and_then(Value::as_str) != Some("complete_task")
}

fn is_function_call(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("function_call")
}

fn validate_schema_completion(
    result_schema: &ClosedSchema,
    outcome: &str,
    fields: &Value,
) -> Result<(), HarnessExecutionError> {
    let (outcome_schema, outcome_fields) = completion_contract(result_schema)?;
    outcome_schema
        .validate(&Value::String(outcome.to_owned()))
        .map_err(|error| HarnessExecutionError::InvalidResult(error.to_string()))?;
    let fields_schema = outcome_fields.get(outcome).ok_or_else(|| {
        HarnessExecutionError::InvalidResult("completion outcome schema is absent".to_owned())
    })?;
    fields_schema.validate(fields).map_err(|error| {
        HarnessExecutionError::InvalidResult(format!(
            "completion fields do not match selected outcome: {error}"
        ))
    })
}

fn completion_contract(
    result_schema: &ClosedSchema,
) -> Result<
    (
        &ClosedSchema,
        &std::collections::BTreeMap<String, ClosedSchema>,
    ),
    HarnessExecutionError,
> {
    let ClosedSchema::Object { properties, .. } = result_schema else {
        return Err(HarnessExecutionError::InvalidResult(
            "completion result schema must be an object".to_owned(),
        ));
    };
    let outcome_schema = properties.get("outcome").ok_or_else(|| {
        HarnessExecutionError::InvalidResult("completion outcome schema is absent".to_owned())
    })?;
    let outcome_fields = properties.get("fields").ok_or_else(|| {
        HarnessExecutionError::InvalidResult("completion fields schema is absent".to_owned())
    })?;
    let ClosedSchema::Object {
        properties: outcome_fields,
        ..
    } = outcome_fields
    else {
        return Err(HarnessExecutionError::InvalidResult(
            "completion fields schema must map outcomes".to_owned(),
        ));
    };
    Ok((outcome_schema, outcome_fields))
}

fn completion_tool_from_schema(result_schema: &ClosedSchema) -> Value {
    let (outcome, fields) = match completion_contract(result_schema) {
        Ok((outcome, outcomes)) => {
            let field_properties = outcomes
                .values()
                .filter_map(|schema| match schema {
                    ClosedSchema::Object { properties, .. } => Some(properties),
                    _ => None,
                })
                .flat_map(|properties| properties.iter())
                .map(|(name, schema)| (name.clone(), model_schema_json(schema)))
                .collect::<Map<_, _>>();
            let required = if outcomes.len() == 1 {
                field_properties.keys().cloned().collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            (
                model_schema_json(outcome),
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": field_properties,
                    "required": required,
                }),
            )
        }
        Err(_) => (
            json!({"type": "string", "enum": []}),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {},
                "required": [],
            }),
        ),
    };
    json!({
        "type": "function",
        "name": "complete_task",
        "description": "Complete the current task with one declared outcome.",
        "parameters": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "outcome": outcome,
                "fields": fields,
            },
            "required": ["outcome", "fields"],
        },
    })
}

fn model_schema_json(schema: &ClosedSchema) -> Value {
    match schema {
        ClosedSchema::Boolean => json!({"type": "boolean"}),
        ClosedSchema::Integer => json!({"type": "integer"}),
        ClosedSchema::Number => json!({"type": "number"}),
        ClosedSchema::String => json!({"type": "string"}),
        ClosedSchema::Array(items) => json!({"type": "array", "items": model_schema_json(items)}),
        ClosedSchema::Object { properties, .. } => json!({
            "type": "object",
            "additionalProperties": false,
            "properties": properties.iter().map(|(name, schema)| {
                (name.clone(), model_schema_json(schema))
            }).collect::<Map<_, _>>(),
            "required": properties.keys().collect::<Vec<_>>(),
        }),
        ClosedSchema::Enum(values) => {
            let values = values
                .iter()
                .map(|value| match value {
                    PrimitiveValue::Boolean(value) => Value::Bool(*value),
                    PrimitiveValue::Integer(value) => Value::Number((*value).into()),
                    PrimitiveValue::String(value) => Value::String(value.clone()),
                })
                .collect::<Vec<_>>();
            let kind = match values.first() {
                Some(Value::Bool(_)) => "boolean",
                Some(Value::Number(_)) => "integer",
                _ => "string",
            };
            json!({"type": kind, "enum": values})
        }
        ClosedSchema::Nullable(inner) => {
            json!({"anyOf": [model_schema_json(inner), {"type": "null"}]})
        }
    }
}

fn parse_completion_item(item: &Value) -> Option<(String, Value)> {
    if item.get("type")?.as_str()? != "function_call"
        || item.get("name")?.as_str()? != "complete_task"
    {
        return None;
    }
    let arguments = item.get("arguments")?;
    let arguments = arguments
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| arguments.clone());
    Some((
        arguments.get("outcome")?.as_str()?.to_owned(),
        arguments.get("fields")?.clone(),
    ))
}

fn harness_tool_json(tool: &HarnessTool) -> Value {
    json!({
        "type": "function",
        "name": tool.qualified_name,
        "description": tool.descriptor.get("description").and_then(Value::as_str).unwrap_or(""),
        "parameters": tool.descriptor.get("input").cloned().unwrap_or_else(|| json!({"type":"object", "properties":{}, "additionalProperties":false})),
    })
}

pub(crate) fn completion_tool(task: &CompiledTask) -> Value {
    json!({"type":"function", "name":"complete_task", "description":"Complete the current task with one declared outcome.", "parameters":{"type":"object", "additionalProperties":false, "properties":{"outcome":{"type":"string"}, "fields":{"type":"object"}}, "required":["outcome", "fields"]}, "outcomes": task.completion.outcomes})
}

pub(crate) fn parse_completion_item_legacy(item: &Value) -> Option<(String, Value)> {
    parse_completion_item(item)
}

pub(crate) fn validate_completion(
    task: &CompiledTask,
    outcome: &str,
    fields: &Value,
) -> Result<(), StoreError> {
    let expected = task.completion.outcomes.get(outcome).ok_or_else(|| {
        StoreError::InvalidData("model selected an undeclared outcome".to_owned())
    })?;
    let actual = fields.as_object().ok_or_else(|| {
        StoreError::InvalidData("model completion fields must be an object".to_owned())
    })?;
    if actual.len() != expected.len() || expected.keys().any(|field| !actual.contains_key(field)) {
        return Err(StoreError::InvalidData(
            "model completion fields do not match the selected outcome".to_owned(),
        ));
    }
    for (name, ty) in expected {
        if !matches_type(&actual[name], ty) {
            return Err(StoreError::InvalidData(format!(
                "model completion field has the wrong type: {name}"
            )));
        }
    }
    Ok(())
}

fn matches_type(value: &Value, ty: &CompiledType) -> bool {
    match ty {
        CompiledType::String => value.is_string(),
        CompiledType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        CompiledType::Number => value.is_number(),
        CompiledType::Boolean => value.is_boolean(),
        CompiledType::Optional(inner) => value.is_null() || matches_type(value, inner),
        CompiledType::List(inner) => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| matches_type(item, inner))),
        CompiledType::Record(_) => value.is_object(),
    }
}
