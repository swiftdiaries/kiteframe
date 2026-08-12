mod transition;

pub use transition::{FrameSnapshot, TransitionError, next_command};

use std::sync::Arc;

use thiserror::Error;
use serde_json::{Value, json};

use crate::{
    IdempotencyKey, ModelClient, ModelRequest, NewPermissionRequest, PermissionRequestId,
    PolicyDecision, PolicyRequest, PostgresStore, SessionAuthorizer, StoreError, ToolCall,
    ToolResolver, canonical_sha256,
};
use crate::dsl::CompiledType;

pub struct Engine {
    pub store: Arc<PostgresStore>,
    pub model: Arc<dyn ModelClient>,
    pub tools: Arc<dyn ToolResolver>,
    pub authorizer: Arc<dyn SessionAuthorizer>,
    pub config: EngineConfig,
}

#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub scheduler_batch_size: i64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { scheduler_batch_size: 32 }
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
}

impl Engine {
    pub async fn tick_until_idle(&self) -> Result<usize, EngineError> {
        let frames = self.store.runnable_frames(self.config.scheduler_batch_size).await?;
        self.tick_frames(frames).await
    }

    pub async fn tick_run(&self, run_id: crate::RunId) -> Result<usize, EngineError> {
        let frames = self.store.runnable_frames_for_run(run_id, self.config.scheduler_batch_size).await?;
        self.tick_frames(frames).await
    }

    async fn tick_frames(&self, frames: Vec<crate::RunnableFrame>) -> Result<usize, EngineError> {
        let mut applied = 0;
        for frame in frames {
            let Some(outcome) = frame.outcome_name else {
                self.run_model_turn(frame).await?;
                applied += 1;
                continue;
            };
            let bindings = frame.bindings.as_object().cloned().ok_or_else(|| StoreError::InvalidData("runnable frame bindings are malformed".to_owned()))?.into_iter().collect();
            let command = next_command(&frame.workflow, &FrameSnapshot { task_name: frame.task_name, outcome, statement_index: frame.statement_index, bindings })?;
            self.store.apply_frame_command(frame.id, frame.revision, command).await?;
            applied += 1;
        }
        Ok(applied)
    }

    async fn run_model_turn(&self, frame: crate::RunnableFrame) -> Result<(), EngineError> {
        let task = frame.workflow.tasks.get(&frame.task_name).ok_or_else(|| StoreError::InvalidData("runnable frame task is absent from workflow".to_owned()))?;
        let attempt = self.store.begin_model_attempt(frame.id.clone(), frame.revision, json!({"bindings": frame.bindings})).await?;
        let mut input = self.store.frame_history(frame.id.clone()).await?;
        input.insert(0, json!({"bindings": frame.bindings}));
        let response = self.model.respond(ModelRequest {
            instructions: task.prompt.clone(),
            input,
            tools: std::iter::once(completion_tool(task)).chain(self.store.run_tool_bindings(frame.run_id.clone()).await?.into_iter().map(model_tool)).collect(),
        }).await.map_err(|error| StoreError::InvalidData(error.to_string()))?;
        self.store.commit_model_attempt(frame.id.clone(), attempt.attempt, attempt.revision, response.items.clone()).await?;
        if let Some(completion) = response.items.iter().filter_map(completion_from_item).next_back() {
            validate_completion(task, &completion.0, &completion.1)?;
            self.store.record_completion(frame.id, attempt.revision + 1, completion.0, completion.1).await?;
        } else if let Some((provider_call_id, qualified_name, arguments)) = response.items.iter().filter_map(tool_call_from_item).next_back() {
            self.invoke_authorized_tool(&frame, attempt.revision + 1, provider_call_id, qualified_name, arguments).await?;
        } else {
            return Err(StoreError::InvalidData("model response did not contain complete_task or a tool call".to_owned()).into());
        }
        Ok(())
    }

    async fn invoke_authorized_tool(
        &self,
        frame: &crate::RunnableFrame,
        expected_revision: u64,
        provider_call_id: String,
        qualified_name: String,
        arguments: Value,
    ) -> Result<(), EngineError> {
        let binding = self.store.run_tool_binding(frame.run_id.clone(), &qualified_name).await?;
        let policy_revision = self.store.policy_revision(frame.session_id.clone()).await?;
        let intent = self.store.begin_tool_call(
            frame.id.clone(), expected_revision, provider_call_id.clone(), qualified_name.clone(),
            binding.descriptor_digest.clone(), arguments.clone(), format!("{}:{}", frame.id.as_uuid(), provider_call_id),
        ).await.map_err(|error| StoreError::InvalidData(format!("persist tool intent: {error}")))?;
        match self.authorizer.authorize(&PolicyRequest {
            session: frame.session_id.clone(), qualified_name: binding.qualified_name.clone(),
            descriptor_digest: binding.descriptor_digest.clone(), capabilities: binding.capabilities,
        }).await.map_err(|error| StoreError::InvalidData(error.to_string()))? {
            PolicyDecision::Allow => {
                let tool = self.tools.bound_tool(frame.session_id.clone(), &binding.qualified_name, &binding.descriptor_digest)
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let call = ToolCall::new(
                    frame.session_id.clone(), frame.run_id.clone(), frame.id.clone(), policy_revision,
                    tool.descriptor(), arguments, IdempotencyKey::new(format!("{}:{}", frame.id.as_uuid(), intent.id)).map_err(|error| StoreError::InvalidData(error.to_string()))?,
                ).map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let outcome = tool.invoke(call)
                    .await.map_err(|error| json!({"code":"tool_failed","message":error.to_string()}));
                self.store.finish_tool_call(frame.id.clone(), intent.id, intent.revision, outcome).await.map_err(|error| StoreError::InvalidData(format!("persist tool result: {error}")))?;
            }
            PolicyDecision::Forbid { .. } => {
                self.store.finish_tool_call(frame.id.clone(), intent.id, intent.revision, Err(json!({"code":"policy_denied","message":"Tool invocation denied by session policy"}))).await.map_err(|error| StoreError::InvalidData(format!("persist denied tool result: {error}")))?;
            }
            PolicyDecision::NeedsPermission => {
                self.store.create_permission_request(frame.id.clone(), intent.revision, NewPermissionRequest {
                    id: PermissionRequestId::new(uuid::Uuid::now_v7()), provider_call_id,
                    qualified_tool: binding.qualified_name, descriptor_digest: binding.descriptor_digest,
                    input_digest: canonical_sha256(&arguments).map_err(|error| StoreError::InvalidData(error.to_string()))?, policy_revision,
                }).await?;
            }
        }
        Ok(())
    }
}

fn completion_tool(task: &crate::dsl::CompiledTask) -> Value {
    json!({"type":"function", "name":"complete_task", "description":"Complete the current task with one declared outcome.", "parameters":{"type":"object", "additionalProperties":false, "properties":{"outcome":{"type":"string"}, "fields":{"type":"object"}}, "required":["outcome", "fields"]}, "outcomes": task.completion.outcomes})
}

fn model_tool(binding: crate::RunToolBinding) -> Value {
    json!({"type":"function", "name": binding.qualified_name, "description": binding.descriptor.get("description").and_then(Value::as_str).unwrap_or(""), "parameters": binding.descriptor.get("input").cloned().unwrap_or_else(|| json!({"type":"object","properties":{},"additionalProperties":false}))})
}

fn completion_from_item(item: &Value) -> Option<(String, Value)> {
    if item.get("type")?.as_str()? != "function_call" || item.get("name")?.as_str()? != "complete_task" { return None; }
    let arguments = item.get("arguments")?;
    let arguments = arguments.as_str().and_then(|text| serde_json::from_str(text).ok()).unwrap_or_else(|| arguments.clone());
    Some((arguments.get("outcome")?.as_str()?.to_owned(), arguments.get("fields")?.clone()))
}

fn tool_call_from_item(item: &Value) -> Option<(String, String, Value)> {
    if item.get("type")?.as_str()? != "function_call" || item.get("name")?.as_str()? == "complete_task" { return None; }
    let arguments = item.get("arguments")?;
    let arguments = arguments.as_str().and_then(|text| serde_json::from_str(text).ok()).unwrap_or_else(|| arguments.clone());
    Some((item.get("call_id").or_else(|| item.get("id"))?.as_str()?.to_owned(), item.get("name")?.as_str()?.to_owned(), arguments))
}

fn validate_completion(task: &crate::dsl::CompiledTask, outcome: &str, fields: &Value) -> Result<(), StoreError> {
    let expected = task.completion.outcomes.get(outcome).ok_or_else(|| StoreError::InvalidData("model selected an undeclared outcome".to_owned()))?;
    let actual = fields.as_object().ok_or_else(|| StoreError::InvalidData("model completion fields must be an object".to_owned()))?;
    if actual.len() != expected.len() || expected.keys().any(|field| !actual.contains_key(field)) {
        return Err(StoreError::InvalidData("model completion fields do not match the selected outcome".to_owned()));
    }
    for (name, ty) in expected {
        if !matches_type(&actual[name], ty) {
            return Err(StoreError::InvalidData(format!("model completion field has the wrong type: {name}")));
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
        CompiledType::List(inner) => value.as_array().is_some_and(|items| items.iter().all(|item| matches_type(item, inner))),
        CompiledType::Record(_) => value.is_object(),
    }
}

#[cfg(test)]
mod tests {
    use crate::dsl::{Limits, compile};
    use crate::{SessionId, ToolCatalogSnapshot};
    use serde_json::json;
    use uuid::Uuid;

    use super::validate_completion;

    fn task() -> crate::dsl::CompiledTask {
        let catalog = ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(4), [])
            .expect("catalog");
        compile("workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok(answer: string) { done answer } }", &catalog, &Limits::default())
            .expect("compile").tasks.remove("t").expect("task")
    }

    #[test]
    fn completion_validation_requires_exact_declared_fields() {
        let task = task();
        assert!(validate_completion(&task, "ok", &json!({"answer":"x"})).is_ok());
        assert!(validate_completion(&task, "unknown", &json!({"answer":"x"})).is_err());
        assert!(validate_completion(&task, "ok", &json!({"answer":1})).is_err());
        assert!(validate_completion(&task, "ok", &json!({"answer":"x", "extra":true})).is_err());
    }

    #[test]
    fn parses_non_completion_function_calls() {
        assert_eq!(
            super::tool_call_from_item(&json!({"type":"function_call","call_id":"c1","name":"docs/search","arguments":"{\"query\":\"x\"}"})),
            Some(("c1".to_owned(), "docs/search".to_owned(), json!({"query":"x"}))),
        );
        assert!(super::tool_call_from_item(&json!({"type":"function_call","call_id":"c1","name":"complete_task","arguments":{}})).is_none());
    }
}
