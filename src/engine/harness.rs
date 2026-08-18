use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::dsl::{CompiledTask, CompiledType};
use crate::{
    AgentCommandState, AgentKernel, ClaimAgentCommand, ClosedSchema, CompleteAgentCommand,
    EvidenceId, FailAgentCommand, GoalPhase, HarnessStructuralLimits, HarnessTool, IdempotencyKey,
    InvokeHarness, KernelCommandPayload, ModelClient, ModelRequest, OperationalStatus,
    PrimitiveValue, RuntimeEvent, RuntimeHistoryItem, RuntimeHistoryKind, SessionId, StoreError,
    TaskLifecycle,
};

const BUILTIN_CLAIMANT: &str = "builtin-direct-v1";
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

#[async_trait::async_trait]
pub trait HarnessCommandExecutor: Send + Sync {
    async fn execute(
        &self,
        invocation: &InvokeHarness,
    ) -> Result<HarnessExecutionResult, HarnessExecutionError>;
}

pub struct DirectLoopHarnessExecutor {
    model: Arc<dyn ModelClient>,
}

impl DirectLoopHarnessExecutor {
    #[must_use]
    pub fn new(model: Arc<dyn ModelClient>) -> Self {
        Self { model }
    }
}

#[async_trait::async_trait]
impl HarnessCommandExecutor for DirectLoopHarnessExecutor {
    async fn execute(
        &self,
        invocation: &InvokeHarness,
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
    let ClosedSchema::Object { properties, .. } = result_schema else {
        return Err(HarnessExecutionError::InvalidResult(
            "completion result schema must be an object".to_owned(),
        ));
    };
    let outcome_schema = properties.get("outcome").ok_or_else(|| {
        HarnessExecutionError::InvalidResult("completion outcome schema is absent".to_owned())
    })?;
    outcome_schema
        .validate(&Value::String(outcome.to_owned()))
        .map_err(|error| HarnessExecutionError::InvalidResult(error.to_string()))?;
    let actual = fields.as_object().ok_or_else(|| {
        HarnessExecutionError::InvalidResult("completion fields must be an object".to_owned())
    })?;
    if actual.keys().any(|field| !properties.contains_key(field)) {
        return Err(HarnessExecutionError::InvalidResult(
            "completion fields contain an undeclared field".to_owned(),
        ));
    }
    if matches!(outcome_schema, ClosedSchema::Enum(values) if values.len() == 1)
        && actual.len() != properties.len().saturating_sub(1)
    {
        return Err(HarnessExecutionError::InvalidResult(
            "completion fields do not match the selected outcome".to_owned(),
        ));
    }
    let mut normalized = Map::from_iter([("outcome".to_owned(), Value::String(outcome.into()))]);
    for (name, schema) in properties {
        if name == "outcome" {
            continue;
        }
        let value = actual.get(name).cloned().unwrap_or(Value::Null);
        schema
            .validate(&value)
            .map_err(|error| HarnessExecutionError::InvalidResult(error.to_string()))?;
        normalized.insert(name.clone(), value);
    }
    result_schema
        .validate(&Value::Object(normalized))
        .map_err(|error| HarnessExecutionError::InvalidResult(error.to_string()))
}

pub struct AgentCommandDispatcher {
    kernel: Arc<AgentKernel>,
    executor: Arc<dyn HarnessCommandExecutor>,
}

impl AgentKernel {
    #[must_use]
    pub fn with_harness_executor(
        self: Arc<Self>,
        executor: Arc<dyn HarnessCommandExecutor>,
    ) -> AgentCommandDispatcher {
        AgentCommandDispatcher {
            kernel: self,
            executor,
        }
    }
}

impl AgentCommandDispatcher {
    pub async fn tick_agent_commands(&self, limit: i64) -> Result<usize, StoreError> {
        let listed = self
            .kernel
            .store()
            .dispatchable_agent_commands(limit)
            .await?;
        let mut processed = 0;
        for listed in listed {
            let session_id = SessionId::new(
                sqlx::query_scalar("SELECT session_id FROM user_tasks WHERE id = $1")
                    .bind(listed.task_id.as_uuid())
                    .fetch_one(self.kernel.store().pool())
                    .await?,
            );
            let context = self
                .kernel
                .store()
                .load_agent_command_context(session_id, listed.task_id.clone(), listed.id.clone())
                .await?;
            if context.execution.lifecycle != TaskLifecycle::Running
                || context.execution.goal_phase != GoalPhase::Active
                || matches!(
                    context.execution.operational_status,
                    OperationalStatus::Paused
                        | OperationalStatus::Blocked
                        | OperationalStatus::Finished
                )
            {
                continue;
            }
            let claimed = match listed.state {
                AgentCommandState::Pending => {
                    let request = ClaimAgentCommand {
                        session_id: context.execution.task.session_id.clone(),
                        task_id: listed.task_id.clone(),
                        command_id: listed.id.clone(),
                        expected_command_revision: listed.revision,
                        claimant: BUILTIN_CLAIMANT.to_owned(),
                        idempotency_key: idempotency_key(format!(
                            "{BUILTIN_CLAIMANT}:{}",
                            listed.id.as_uuid()
                        ))?,
                    };
                    match self.kernel.store().claim_agent_command(request).await {
                        Ok(claimed) => claimed,
                        Err(StoreError::RevisionConflict) => continue,
                        Err(error) => return Err(error),
                    }
                }
                AgentCommandState::Claimed
                    if listed.claimant.as_deref() == Some(BUILTIN_CLAIMANT) =>
                {
                    listed
                }
                AgentCommandState::Claimed
                | AgentCommandState::Completed
                | AgentCommandState::Failed
                | AgentCommandState::Cancelled => continue,
            };
            self.dispatch_claimed(&context.execution, claimed).await?;
            processed += 1;
        }
        Ok(processed)
    }

    async fn dispatch_claimed(
        &self,
        execution: &crate::AgentExecutionView,
        claimed: crate::AgentCommandView,
    ) -> Result<(), StoreError> {
        match dispatch_route(&claimed.payload) {
            DispatchRoute::Harness => {
                let KernelCommandPayload::InvokeHarness(invocation) = &claimed.payload else {
                    unreachable!("harness route must contain InvokeHarness")
                };
                match self.executor.execute(invocation).await {
                    Ok(result) => {
                        let completion = CompleteAgentCommand {
                            session_id: execution.task.session_id.clone(),
                            task_id: claimed.task_id.clone(),
                            command_id: claimed.id.clone(),
                            expected_command_revision: claimed.revision,
                            idempotency_key: idempotency_key(format!(
                                "builtin-result:{}:{}",
                                claimed.id.as_uuid(),
                                claimed.revision
                            ))?,
                            event: RuntimeEvent::HarnessResult {
                                event_id: format!(
                                    "builtin-harness-result:{}:{}",
                                    claimed.id.as_uuid(),
                                    claimed.revision
                                ),
                                command_key: claimed.key.clone(),
                                outcome: result.outcome,
                                fields: result.fields,
                                evidence_id: EvidenceId::new(uuid::Uuid::now_v7()),
                                history: result.history,
                            },
                        };
                        ignore_late_terminal(
                            self.kernel
                                .complete_command(&execution.task.principal, completion)
                                .await,
                        )?;
                    }
                    Err(HarnessExecutionError::Unavailable(_)) => {
                        let failure = failure_request(
                            execution,
                            &claimed,
                            "builtin-unavailable",
                            "runtime_command_unavailable",
                        )?;
                        ignore_late_terminal(
                            self.kernel
                                .store()
                                .fail_unavailable_agent_command(&execution.task.principal, &failure)
                                .await,
                        )?;
                    }
                    Err(
                        HarnessExecutionError::InvalidResult(_) | HarnessExecutionError::Failed(_),
                    ) => {
                        let failure = failure_request(
                            execution,
                            &claimed,
                            "builtin-failure",
                            "harness_failed",
                        )?;
                        ignore_late_terminal(
                            self.kernel
                                .fail_command(&execution.task.principal, failure)
                                .await,
                        )?;
                    }
                }
            }
            DispatchRoute::Unavailable => {
                let failure = failure_request(
                    execution,
                    &claimed,
                    "builtin-unavailable",
                    "runtime_command_unavailable",
                )?;
                ignore_late_terminal(
                    self.kernel
                        .store()
                        .fail_unavailable_agent_command(&execution.task.principal, &failure)
                        .await,
                )?;
            }
            DispatchRoute::InternalAuthority => {
                return Err(StoreError::AgentTransitionInvalid(
                    "internal authority command reached dispatcher".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DispatchRoute {
    Harness,
    Unavailable,
    InternalAuthority,
}

fn dispatch_route(payload: &KernelCommandPayload) -> DispatchRoute {
    match payload {
        KernelCommandPayload::InvokeHarness(_) => DispatchRoute::Harness,
        // RequestApproval has a canonical Slice 2 wait. Context and event requests remain
        // defensive only until their canonical runtime states and resolution events exist.
        KernelCommandPayload::RequestContext(_)
        | KernelCommandPayload::WaitForEvent(_)
        | KernelCommandPayload::RequestApproval(_) => DispatchRoute::Unavailable,
        KernelCommandPayload::CompleteGoal(_)
        | KernelCommandPayload::BlockGoal(_)
        | KernelCommandPayload::FailTask(_) => DispatchRoute::InternalAuthority,
    }
}

fn failure_request(
    execution: &crate::AgentExecutionView,
    claimed: &crate::AgentCommandView,
    prefix: &str,
    code: &str,
) -> Result<FailAgentCommand, StoreError> {
    Ok(FailAgentCommand {
        session_id: execution.task.session_id.clone(),
        task_id: claimed.task_id.clone(),
        command_id: claimed.id.clone(),
        expected_command_revision: claimed.revision,
        idempotency_key: idempotency_key(format!(
            "{prefix}:{}:{}",
            claimed.id.as_uuid(),
            claimed.revision
        ))?,
        code: code.to_owned(),
    })
}

fn idempotency_key(value: String) -> Result<IdempotencyKey, StoreError> {
    IdempotencyKey::new(value).map_err(|error| StoreError::InvalidData(error.to_string()))
}

fn ignore_late_terminal(
    result: Result<crate::AgentExecutionView, StoreError>,
) -> Result<(), StoreError> {
    match result {
        Ok(_) | Err(StoreError::AgentTaskAlreadyTerminal | StoreError::GoalAlreadyTerminal) => {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn completion_tool(task: &CompiledTask) -> Value {
    json!({"type":"function", "name":"complete_task", "description":"Complete the current task with one declared outcome.", "parameters":{"type":"object", "additionalProperties":false, "properties":{"outcome":{"type":"string"}, "fields":{"type":"object"}}, "required":["outcome", "fields"]}, "outcomes": task.completion.outcomes})
}

pub(crate) fn completion_tool_from_schema(result_schema: &ClosedSchema) -> Value {
    let (outcome, fields) = match result_schema {
        ClosedSchema::Object { properties, .. } => {
            let outcome = properties
                .get("outcome")
                .map(model_schema_json)
                .unwrap_or_else(|| json!({"type": "string", "enum": []}));
            let field_properties = properties
                .iter()
                .filter(|(name, _)| name.as_str() != "outcome")
                .map(|(name, schema)| (name.clone(), model_schema_json(schema)))
                .collect::<Map<_, _>>();
            let required = match properties.get("outcome") {
                Some(ClosedSchema::Enum(values)) if values.len() == 1 => {
                    field_properties.keys().cloned().collect::<Vec<_>>()
                }
                _ => Vec::new(),
            };
            (
                outcome,
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "properties": field_properties,
                    "required": required,
                }),
            )
        }
        _ => (
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
        ClosedSchema::Array(items) => {
            json!({"type": "array", "items": model_schema_json(items)})
        }
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

pub(crate) fn parse_completion_item(item: &Value) -> Option<(String, Value)> {
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

fn harness_tool_json(tool: &HarnessTool) -> Value {
    json!({
        "type": "function",
        "name": tool.qualified_name,
        "description": tool.descriptor.get("description").and_then(Value::as_str).unwrap_or(""),
        "parameters": tool.descriptor.get("input").cloned().unwrap_or_else(|| json!({"type":"object", "properties":{}, "additionalProperties":false})),
    })
}

#[cfg(test)]
mod tests {
    use super::{DispatchRoute, dispatch_route};
    use crate::{KernelCommandPayload, RequestContext, WaitForEvent};
    use serde_json::json;

    #[test]
    fn context_and_event_commands_are_defensively_classified_as_unavailable() {
        for payload in [
            KernelCommandPayload::RequestContext(RequestContext {
                evidence_type: "github.context".to_owned(),
                subject: json!({}),
            }),
            KernelCommandPayload::WaitForEvent(WaitForEvent {
                event_type: "github.changed.v1".to_owned(),
                correlation: json!({}),
            }),
        ] {
            assert_eq!(dispatch_route(&payload), DispatchRoute::Unavailable);
        }
    }
}
