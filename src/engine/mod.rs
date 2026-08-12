mod transition;
mod recovery;

pub use transition::{FrameSnapshot, TransitionError, next_command};
pub use recovery::RecoveryReport;

use std::sync::Arc;
use std::collections::BTreeSet;

use thiserror::Error;
use serde::Deserialize;
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

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionChoice {
    AllowOnce,
    AllowToolForSession,
    DenyOnce,
    DenyToolForSession,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResolvePermission {
    pub session_id: crate::SessionId,
    pub run_id: crate::RunId,
    pub permission_request_id: crate::PermissionRequestId,
    pub choice: PermissionChoice,
    pub idempotency_key: crate::IdempotencyKey,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AnswerQuestion {
    pub session_id: crate::SessionId,
    pub run_id: crate::RunId,
    pub question_id: uuid::Uuid,
    pub answer: Value,
    pub idempotency_key: crate::IdempotencyKey,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CancelRun {
    pub session_id: crate::SessionId,
    pub run_id: crate::RunId,
    pub reason: Option<String>,
    pub idempotency_key: crate::IdempotencyKey,
}

#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub scheduler_batch_size: i64,
    pub status_poll_retry_budget: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { scheduler_batch_size: 32, status_poll_retry_budget: 3 }
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
    pub async fn recover(&self) -> Result<RecoveryReport, EngineError> {
        let mut report = recovery::recover_model_attempts(&self.store).await?;
        let tools = recovery::recover_tool_statuses(
            &self.store,
            self.tools.as_ref(),
            self.config.status_poll_retry_budget,
        )
        .await?;
        report.tools_recovered = tools.tools_recovered;
        report.tools_requeued = tools.tools_requeued;
        report.sessions_waiting_for_mcp = tools.sessions_waiting_for_mcp;
        report.outcomes_unknown = tools.outcomes_unknown;
        Ok(report)
    }

    pub async fn cancel_run(&self, request: CancelRun) -> Result<(), EngineError> {
        let revision = self.store.run_revision(request.session_id, request.run_id.clone()).await?;
        self.store.cancel_run(request.run_id, revision, request.reason, request.idempotency_key.as_str().to_owned()).await?;
        Ok(())
    }

    pub async fn resolve_permission(&self, request: ResolvePermission) -> Result<(), EngineError> {
        let resolution_key = permission_resolution_key(&request.idempotency_key, request.choice)?;
        let pending = self
            .store
            .pending_permission(
                request.session_id.clone(),
                request.run_id.clone(),
                request.permission_request_id.clone(),
            )
            .await?;
        if pending.resolved {
            return if pending.resolution_idempotency_key.as_deref()
                == Some(resolution_key.as_str())
            {
                Ok(())
            } else {
                Err(StoreError::IdempotencyConflict.into())
            };
        }
        let policy_update = if matches!(
            request.choice,
            PermissionChoice::AllowToolForSession | PermissionChoice::DenyToolForSession
        ) {
            let catalog = self
                .store
                .current_catalog(request.session_id.clone())
                .await?;
            catalog
                .resolve(&pending.qualified_name, &pending.descriptor_digest)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?;
            let (mut policy, revision) = self
                .store
                .current_session_policy_snapshot(request.session_id.clone())
                .await?;
            let rule_id = format!("permission-{}", request.permission_request_id.as_uuid());
            if policy.rules.iter().any(|rule| rule.rule_id == rule_id) {
                let resolved = self.store.pending_permission(
                    request.session_id.clone(),
                    request.run_id.clone(),
                    request.permission_request_id.clone(),
                ).await?;
                return if resolved.resolution_idempotency_key.as_deref()
                    == Some(resolution_key.as_str())
                {
                    Ok(())
                } else {
                    Err(StoreError::IdempotencyConflict.into())
                };
            }
            policy.rules.push(crate::PolicyRule {
                rule_id,
                effect: match request.choice {
                    PermissionChoice::AllowToolForSession => crate::PolicyEffect::Permit,
                    PermissionChoice::DenyToolForSession => crate::PolicyEffect::Forbid,
                    _ => unreachable!(),
                },
                target: crate::PolicyTarget::Tool {
                    names: BTreeSet::from([pending.qualified_name.clone()]),
                },
                input: Default::default(),
            });
            let compiled = crate::policy::compile(&policy, &catalog)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?;
            Some(crate::PolicyUpdate {
                session: request.session_id.clone(),
                expected_revision: revision,
                idempotency_key: resolution_key.clone(),
                typed_source: serde_json::to_value(policy)
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?,
                compiled_cedar: serde_json::to_string(&compiled)
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?,
            })
        } else {
            None
        };
        let applied = self.store.resolve_permission_with_policy(
            pending.frame_id.clone(), request.permission_request_id, pending.revision,
            json!({"choice": match request.choice { PermissionChoice::AllowOnce => "allow_once", PermissionChoice::AllowToolForSession => "allow_tool_for_session", PermissionChoice::DenyOnce => "deny_once", PermissionChoice::DenyToolForSession => "deny_tool_for_session" }}),
            resolution_key,
            policy_update,
        ).await?;
        if !applied { return Ok(()); }
        let revision = pending.revision + 1;
        match request.choice {
            PermissionChoice::AllowOnce | PermissionChoice::AllowToolForSession => {
                let binding = self.store.run_tool_binding(request.run_id.clone(), &pending.qualified_name).await?;
                let tool = self.tools.bound_tool(request.session_id.clone(), &binding.qualified_name, &binding.descriptor_digest).map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let policy_revision = self.store.policy_revision(request.session_id.clone()).await?;
                let call = ToolCall::new(request.session_id, request.run_id, pending.frame_id.clone(), policy_revision, tool.descriptor(), pending.arguments, IdempotencyKey::new(format!("{}:{}", pending.frame_id.as_uuid(), pending.provider_call_id)).map_err(|error| StoreError::InvalidData(error.to_string()))?).map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let result = tool.invoke(call).await.map_err(|error| json!({"code":"tool_failed","message":error.to_string()}));
                self.store.finish_tool_call(pending.frame_id, pending.tool_call_id, revision, result).await?;
            }
            PermissionChoice::DenyOnce | PermissionChoice::DenyToolForSession => self.store.finish_tool_call(pending.frame_id, pending.tool_call_id, revision, Err(json!({"code":"permission_denied","message":"Tool invocation denied by user"}))).await?,
        }
        Ok(())
    }

    pub async fn answer_question(&self, request: AnswerQuestion) -> Result<(), EngineError> {
        let suspension = self.store.question_suspension(request.session_id, request.run_id, request.question_id).await?;
        self.store.answer(suspension.frame_id, request.question_id, suspension.revision, request.answer, request.idempotency_key.as_str().to_owned()).await?;
        Ok(())
    }

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
        let limit = frame.workflow.limits.get("model_turns_per_task").copied().unwrap_or(u64::MAX);
        if self.store.model_attempt_count(frame.id.clone()).await? >= limit {
            return Err(StoreError::InvalidData("model turn limit reached for task".to_owned()).into());
        }
        let attempt = self.store.begin_model_attempt(frame.id.clone(), frame.revision, json!({"bindings": frame.bindings})).await?;
        let mut input = self.store.frame_history(frame.id.clone()).await?;
        input.insert(0, json!({"bindings": frame.bindings}));
        let response = self.model.respond(ModelRequest {
            instructions: task.prompt.clone(),
            input,
            tools: std::iter::once(completion_tool(task)).chain(self.store.run_tool_bindings(frame.run_id.clone()).await?.into_iter().map(model_tool)).collect(),
        }).await.map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let item_limit = frame.workflow.limits.get("response_item_bytes").copied().unwrap_or(u64::MAX);
        if response.items.iter().any(|item| serde_json::to_vec(item).map_or(true, |bytes| bytes.len() as u64 > item_limit)) {
            return Err(StoreError::InvalidData("model response item exceeds workflow byte limit".to_owned()).into());
        }
        let tool_limit = frame.workflow.limits.get("tool_calls_per_response").copied().unwrap_or(u64::MAX);
        let tool_calls = response.items.iter().filter(|item| tool_call_from_item(item).is_some()).count() as u64;
        if tool_calls > tool_limit {
            return Err(StoreError::InvalidData("model response exceeds workflow tool-call limit".to_owned()).into());
        }
        self.store.commit_model_attempt(frame.id.clone(), attempt.attempt, attempt.revision, response.items.clone()).await?;
        let calls = response.items.iter().filter_map(action_from_item).collect::<Vec<_>>();
        let [call] = calls.as_slice() else {
            return Err(StoreError::InvalidData("model response must contain exactly one completion or tool call".to_owned()).into());
        };
        match call {
            ModelAction::Completion(outcome, fields) => {
                validate_completion(task, outcome, fields)?;
                self.store.record_completion(frame.id, attempt.revision + 1, outcome.clone(), fields.clone()).await?;
            }
            ModelAction::ToolCall(provider_call_id, qualified_name, arguments) => {
                self.invoke_authorized_tool(&frame, attempt.revision + 1, provider_call_id.clone(), qualified_name.clone(), arguments.clone()).await?;
            }
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
                    tool.descriptor(), arguments, IdempotencyKey::new(format!("{}:{}", frame.id.as_uuid(), provider_call_id)).map_err(|error| StoreError::InvalidData(error.to_string()))?,
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

fn permission_resolution_key(
    base: &crate::IdempotencyKey,
    choice: PermissionChoice,
) -> Result<String, StoreError> {
    let choice = match choice {
        PermissionChoice::AllowOnce => "allow_once",
        PermissionChoice::AllowToolForSession => "allow_tool_for_session",
        PermissionChoice::DenyOnce => "deny_once",
        PermissionChoice::DenyToolForSession => "deny_tool_for_session",
    };
    Ok(format!("{}:{choice}", base.as_str()))
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

enum ModelAction {
    Completion(String, Value),
    ToolCall(String, String, Value),
}

fn action_from_item(item: &Value) -> Option<ModelAction> {
    completion_from_item(item).map(|(outcome, fields)| ModelAction::Completion(outcome, fields))
        .or_else(|| tool_call_from_item(item).map(|(id, name, arguments)| ModelAction::ToolCall(id, name, arguments)))
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

    #[test]
    fn action_parser_keeps_completion_and_tool_calls_distinct() {
        let actions = [
            json!({"type":"function_call","call_id":"call-1","name":"docs/search","arguments":{}}),
            json!({"type":"function_call","call_id":"done-1","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"x"}}}),
        ].iter().filter_map(super::action_from_item).count();
        assert_eq!(actions, 2);
    }
}
