mod harness;
mod recovery;
mod transition;

pub use crate::harness::{HarnessExecutionError, HarnessExecutionResult};
pub use harness::{AgentCommandDispatcher, HarnessDispatcherError, HarnessRecoveryReport};
#[cfg(feature = "test-support")]
pub use harness::{
    HarnessDispatcherTestBarrier, HarnessDispatcherTestBarrierPoint, HarnessDispatcherTestFault,
    HarnessDispatcherTestFaultPoint,
};
pub use recovery::RecoveryReport;
pub use transition::{FrameSnapshot, TransitionError, next_command};

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    IdempotencyKey, ModelClient, ModelRequest, NewPermissionRequest, PermissionRequestId,
    PolicyDecision, PolicyRequest, PolicyRevision, PostgresStore, RecoverySupport,
    SessionAuthorizer, StoreError, ToolCall, ToolError, ToolResolver, ToolStatus, canonical_sha256,
};

use crate::harness::{completion_tool, parse_completion_item, validate_completion};

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

/// A direct user authorization to repeat a status-proven unstarted tool call.
#[derive(Clone, Debug, Deserialize)]
pub struct RetryRecovery {
    pub session_id: crate::SessionId,
    pub run_id: crate::RunId,
    pub tool_call_id: uuid::Uuid,
}

#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub scheduler_batch_size: i64,
    pub status_poll_retry_budget: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            scheduler_batch_size: 32,
            status_poll_retry_budget: 3,
        }
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
        let revision = self
            .store
            .run_revision(request.session_id, request.run_id.clone())
            .await?;
        self.store
            .cancel_run(
                request.run_id,
                revision,
                request.reason,
                request.idempotency_key.as_str().to_owned(),
            )
            .await?;
        Ok(())
    }

    /// Retries only a provider-confirmed `not_started` intent using its original key.
    pub async fn retry_recovery(&self, request: RetryRecovery) -> Result<(), EngineError> {
        let call = self
            .store
            .recoverable_tool_call(
                request.session_id.clone(),
                request.run_id,
                request.tool_call_id,
            )
            .await?;
        let tool = self
            .tools
            .bound_tool(
                call.session_id.clone(),
                &call.qualified_name,
                &call.descriptor_digest,
            )
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        if !matches!(
            tool.status(&call.idempotency_key).await,
            Ok(ToolStatus::NotStarted)
        ) {
            return Err(StoreError::InvalidData(
                "recovery retry requires provider status not_started".to_owned(),
            )
            .into());
        }
        if !matches!(tool.descriptor().effect(), crate::EffectKind::ReadOnly)
            && !matches!(tool.descriptor().recovery(), RecoverySupport::Idempotent)
        {
            return Err(StoreError::InvalidData(
                "recovery retry requires a read-only or idempotent tool".to_owned(),
            )
            .into());
        }
        let (decision, policy_revision) = self
            .evaluate_policy(PolicyRequest {
                session: call.session_id.clone(),
                qualified_name: call.qualified_name.clone(),
                descriptor_digest: call.descriptor_digest.clone(),
                capabilities: tool.descriptor().capabilities().iter().cloned().collect(),
                arguments: call.arguments.clone(),
            })
            .await?;
        if decision != PolicyDecision::Allow {
            return Err(StoreError::InvalidData(
                "recovery retry is not currently authorized by session policy".to_owned(),
            )
            .into());
        }
        let Some(revision) = self
            .store
            .claim_tool_recovery_retry(
                call.frame_id.clone(),
                call.id,
                call.revision,
                self.config.status_poll_retry_budget,
            )
            .await?
        else {
            return Err(
                StoreError::InvalidData("recovery retry budget is exhausted".to_owned()).into(),
            );
        };
        let invocation = ToolCall::new(
            call.session_id,
            call.run_id,
            call.frame_id.clone(),
            policy_revision,
            tool.descriptor(),
            call.arguments,
            call.idempotency_key,
        )
        .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let outcome = tool
            .invoke(invocation)
            .await
            .map_err(|error| json!({"code":"tool_failed","message":error.to_string()}));
        self.store
            .finish_tool_call(call.frame_id, call.id, revision, outcome)
            .await?;
        Ok(())
    }

    async fn evaluate_policy(
        &self,
        request: PolicyRequest,
    ) -> Result<(PolicyDecision, PolicyRevision), EngineError> {
        let evaluation = self
            .authorizer
            .authorize(&request)
            .await
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let revision = match evaluation.policy_revision {
            Some(revision) => revision,
            None => self.store.policy_revision(request.session).await?,
        };
        Ok((evaluation.decision, revision))
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
            return if pending.resolution_idempotency_key.as_deref() == Some(resolution_key.as_str())
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
                let resolved = self
                    .store
                    .pending_permission(
                        request.session_id.clone(),
                        request.run_id.clone(),
                        request.permission_request_id.clone(),
                    )
                    .await?;
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
        if !applied {
            return Ok(());
        }
        let revision = pending.revision + 1;
        match request.choice {
            PermissionChoice::AllowOnce | PermissionChoice::AllowToolForSession => {
                let binding = self.store.run_tool_binding(request.run_id.clone(), &pending.qualified_name).await?;
                let evaluated = self
                    .evaluate_policy(PolicyRequest {
                        session: request.session_id.clone(),
                        qualified_name: binding.qualified_name.clone(),
                        descriptor_digest: binding.descriptor_digest.clone(),
                        capabilities: binding.capabilities.clone(),
                        arguments: pending.arguments.clone(),
                    })
                    .await;
                let (decision, policy_revision) = match evaluated {
                    Ok(evaluated) => evaluated,
                    Err(error) => {
                        self.store.finish_tool_call(pending.frame_id, pending.tool_call_id, revision, Err(json!({"code":"policy_evaluation_failed","message":error.to_string()}))).await?;
                        return Err(error);
                    }
                };
                if let PolicyDecision::Forbid { .. } = decision {
                    self.store.finish_tool_call(pending.frame_id, pending.tool_call_id, revision, Err(policy_denied())).await?;
                    return Ok(());
                }
                let tool = match self.tools.bound_tool(
                    request.session_id.clone(),
                    &binding.qualified_name,
                    &binding.descriptor_digest,
                ) {
                    Ok(tool) => tool,
                    Err(ToolError::ToolContractChanged { .. } | ToolError::UnknownTool(_)) => {
                        self.store.finish_tool_call(
                            pending.frame_id,
                            pending.tool_call_id,
                            revision,
                            Err(json!({"code":"tool_contract_changed"})),
                        ).await?;
                        return Ok(());
                    }
                    Err(error) => return Err(StoreError::InvalidData(error.to_string()).into()),
                };
                let call = ToolCall::new(request.session_id, request.run_id, pending.frame_id.clone(), policy_revision, tool.descriptor(), pending.arguments, IdempotencyKey::new(format!("{}:{}", pending.frame_id.as_uuid(), pending.provider_call_id)).map_err(|error| StoreError::InvalidData(error.to_string()))?).map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let result = tool.invoke(call).await.map_err(|error| json!({"code":"tool_failed","message":error.to_string()}));
                self.store.finish_tool_call(pending.frame_id, pending.tool_call_id, revision, result).await?;
            }
            PermissionChoice::DenyOnce | PermissionChoice::DenyToolForSession => self.store.finish_tool_call(pending.frame_id, pending.tool_call_id, revision, Err(json!({"code":"permission_denied","message":"Tool invocation denied by user"}))).await?,
        }
        Ok(())
    }

    pub async fn answer_question(&self, request: AnswerQuestion) -> Result<(), EngineError> {
        let suspension = self
            .store
            .question_suspension(request.session_id, request.run_id, request.question_id)
            .await?;
        self.store
            .answer(
                suspension.frame_id,
                request.question_id,
                suspension.revision,
                request.answer,
                request.idempotency_key.as_str().to_owned(),
            )
            .await?;
        Ok(())
    }

    pub async fn tick_until_idle(&self) -> Result<usize, EngineError> {
        let frames = self
            .store
            .runnable_frames(self.config.scheduler_batch_size)
            .await?;
        self.tick_frames(frames).await
    }

    pub async fn tick_run(&self, run_id: crate::RunId) -> Result<usize, EngineError> {
        let frames = self
            .store
            .runnable_frames_for_run(run_id, self.config.scheduler_batch_size)
            .await?;
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
            let bindings = frame
                .bindings
                .as_object()
                .cloned()
                .ok_or_else(|| {
                    StoreError::InvalidData("runnable frame bindings are malformed".to_owned())
                })?
                .into_iter()
                .collect();
            let command = next_command(
                &frame.workflow,
                &FrameSnapshot {
                    task_name: frame.task_name,
                    outcome,
                    statement_index: frame.statement_index,
                    bindings,
                },
            )?;
            let step_limit = frame
                .workflow
                .limits
                .get("steps")
                .copied()
                .unwrap_or(u64::MAX);
            self.store
                .apply_frame_command_with_step_limit(frame.id, frame.revision, command, step_limit)
                .await?;
            applied += 1;
        }
        Ok(applied)
    }

    async fn run_model_turn(&self, frame: crate::RunnableFrame) -> Result<(), EngineError> {
        let task = frame.workflow.tasks.get(&frame.task_name).ok_or_else(|| {
            StoreError::InvalidData("runnable frame task is absent from workflow".to_owned())
        })?;
        let limit = frame
            .workflow
            .limits
            .get("model_turns_per_task")
            .copied()
            .unwrap_or(u64::MAX);
        if self.store.model_attempt_count(frame.id.clone()).await? >= limit {
            self.store
                .fail_frame_with_code(frame.id, frame.revision, "model_turn_limit_exceeded")
                .await?;
            return Ok(());
        }
        let attempt = self
            .store
            .begin_model_attempt(
                frame.id.clone(),
                frame.revision,
                json!({"bindings": frame.bindings}),
            )
            .await?;
        let mut input = self.store.frame_history(frame.id.clone()).await?;
        input.insert(0, json!({"bindings": frame.bindings}));
        let active_limit = active_execution_limit_millis(&frame.workflow);
        let remaining = if active_limit == u64::MAX {
            None
        } else {
            match self
                .store
                .begin_active_execution(frame.run_id.clone(), active_limit)
                .await?
            {
                Some(remaining) => Some(remaining),
                None => {
                    self.store
                        .fail_frame_with_code(
                            frame.id,
                            attempt.revision,
                            "active_execution_limit_exceeded",
                        )
                        .await?;
                    return Ok(());
                }
            }
        };
        let run_tools = match self.store.run_tool_bindings(frame.run_id.clone()).await {
            Ok(bindings) => bindings,
            Err(StoreError::InvalidData(message)) if message == "run tool descriptor is absent" => {
                self.store
                    .fail_frame_with_code(frame.id, attempt.revision, "run_tool_descriptor_missing")
                    .await?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let request = ModelRequest {
            instructions: task.prompt.clone(),
            input,
            tools: std::iter::once(completion_tool(task))
                .chain(run_tools.into_iter().map(model_tool))
                .collect(),
        };
        let response = match remaining {
            Some(remaining) => {
                let result = tokio::time::timeout(
                    std::time::Duration::from_millis(remaining.max(1)),
                    self.model.respond(request),
                )
                .await;
                let exhausted = self
                    .store
                    .finish_active_execution(frame.run_id.clone(), active_limit)
                    .await?;
                if exhausted || result.is_err() {
                    self.store
                        .fail_frame_with_code(
                            frame.id,
                            attempt.revision,
                            "active_execution_limit_exceeded",
                        )
                        .await?;
                    return Ok(());
                }
                result
                    .expect("timeout checked")
                    .map_err(|error| StoreError::InvalidData(error.to_string()))?
            }
            None => self
                .model
                .respond(request)
                .await
                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
        };
        let item_limit = frame
            .workflow
            .limits
            .get("response_item_bytes")
            .copied()
            .unwrap_or(u64::MAX);
        if response.items.iter().any(|item| {
            serde_json::to_vec(item).map_or(true, |bytes| bytes.len() as u64 > item_limit)
        }) {
            self.store
                .fail_frame_with_code(frame.id, attempt.revision, "response_item_limit_exceeded")
                .await?;
            return Ok(());
        }
        let tool_limit = frame
            .workflow
            .limits
            .get("tool_calls_per_response")
            .copied()
            .unwrap_or(u64::MAX);
        let tool_calls = response
            .items
            .iter()
            .filter(|item| tool_call_from_item(item).is_some())
            .count() as u64;
        if tool_calls > tool_limit {
            self.store
                .fail_frame_with_code(frame.id, attempt.revision, "tool_call_limit_exceeded")
                .await?;
            return Ok(());
        }
        let applied = self
            .store
            .commit_model_attempt(
                frame.id.clone(),
                attempt.attempt,
                attempt.revision,
                response.items.clone(),
            )
            .await?;
        if !applied {
            return Ok(());
        }
        let calls = response
            .items
            .iter()
            .filter_map(action_from_item)
            .collect::<Vec<_>>();
        let [call] = calls.as_slice() else {
            return Err(StoreError::InvalidData(
                "model response must contain exactly one completion or tool call".to_owned(),
            )
            .into());
        };
        match call {
            ModelAction::Completion(outcome, fields) => {
                validate_completion(task, outcome, fields)?;
                self.store
                    .record_completion(
                        frame.id,
                        attempt.revision + 1,
                        outcome.clone(),
                        fields.clone(),
                    )
                    .await?;
            }
            ModelAction::ToolCall(provider_call_id, qualified_name, arguments) => {
                self.invoke_authorized_tool(
                    &frame,
                    attempt.revision + 1,
                    provider_call_id.clone(),
                    qualified_name.clone(),
                    arguments.clone(),
                )
                .await?;
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
        let binding = self
            .store
            .run_tool_binding(frame.run_id.clone(), &qualified_name)
            .await?;
        let intent = self
            .store
            .begin_tool_call(
                frame.id.clone(),
                expected_revision,
                provider_call_id.clone(),
                qualified_name.clone(),
                binding.descriptor_digest.clone(),
                arguments.clone(),
                format!("{}:{}", frame.id.as_uuid(), provider_call_id),
            )
            .await
            .map_err(|error| StoreError::InvalidData(format!("persist tool intent: {error}")))?;
        let (decision, policy_revision) = self
            .evaluate_policy(PolicyRequest {
                session: frame.session_id.clone(),
                qualified_name: binding.qualified_name.clone(),
                descriptor_digest: binding.descriptor_digest.clone(),
                capabilities: binding.capabilities,
                arguments: arguments.clone(),
            })
            .await?;
        match decision {
            PolicyDecision::Allow => {
                let tool = match self.tools.bound_tool(
                    frame.session_id.clone(),
                    &binding.qualified_name,
                    &binding.descriptor_digest,
                ) {
                    Ok(tool) => tool,
                    Err(ToolError::ToolContractChanged { .. } | ToolError::UnknownTool(_)) => {
                        self.store
                            .finish_tool_call(
                                frame.id.clone(),
                                intent.id,
                                intent.revision,
                                Err(json!({"code":"tool_contract_changed"})),
                            )
                            .await
                            .map_err(|error| {
                                StoreError::InvalidData(format!(
                                    "persist changed tool contract result: {error}"
                                ))
                            })?;
                        return Ok(());
                    }
                    Err(error) => return Err(StoreError::InvalidData(error.to_string()).into()),
                };
                let call = ToolCall::new(
                    frame.session_id.clone(),
                    frame.run_id.clone(),
                    frame.id.clone(),
                    policy_revision,
                    tool.descriptor(),
                    arguments,
                    IdempotencyKey::new(format!("{}:{}", frame.id.as_uuid(), provider_call_id))
                        .map_err(|error| StoreError::InvalidData(error.to_string()))?,
                )
                .map_err(|error| StoreError::InvalidData(error.to_string()))?;
                let active_limit = active_execution_limit_millis(&frame.workflow);
                let remaining = if active_limit == u64::MAX {
                    None
                } else {
                    match self
                        .store
                        .begin_active_execution(frame.run_id.clone(), active_limit)
                        .await?
                    {
                        Some(remaining) => Some(remaining),
                        None => {
                            self.store
                                .finish_tool_call(
                                    frame.id.clone(),
                                    intent.id,
                                    intent.revision,
                                    Err(json!({"code":"active_execution_limit_exceeded"})),
                                )
                                .await?;
                            self.store
                                .fail_frame_with_code(
                                    frame.id.clone(),
                                    intent.revision + 1,
                                    "active_execution_limit_exceeded",
                                )
                                .await?;
                            return Ok(());
                        }
                    }
                };
                let outcome = match remaining {
                    Some(remaining) => {
                        let result = tokio::time::timeout(
                            std::time::Duration::from_millis(remaining.max(1)),
                            tool.invoke(call),
                        )
                        .await;
                        let exhausted = self
                            .store
                            .finish_active_execution(frame.run_id.clone(), active_limit)
                            .await?;
                        if result.is_err() {
                            self.store
                                .mark_tool_call_outcome_unknown(
                                    frame.id.clone(),
                                    intent.id,
                                    intent.revision,
                                )
                                .await?;
                            return Ok(());
                        }
                        let outcome = result.expect("timeout checked").map_err(
                            |error| json!({"code":"tool_failed","message":error.to_string()}),
                        );
                        if exhausted {
                            self.store
                                .finish_tool_call(
                                    frame.id.clone(),
                                    intent.id,
                                    intent.revision,
                                    outcome,
                                )
                                .await?;
                            self.store
                                .fail_frame_with_code(
                                    frame.id.clone(),
                                    intent.revision + 1,
                                    "active_execution_limit_exceeded",
                                )
                                .await?;
                            return Ok(());
                        }
                        outcome
                    }
                    None => tool
                        .invoke(call)
                        .await
                        .map_err(|error| json!({"code":"tool_failed","message":error.to_string()})),
                };
                self.store
                    .finish_tool_call(frame.id.clone(), intent.id, intent.revision, outcome)
                    .await
                    .map_err(|error| {
                        StoreError::InvalidData(format!("persist tool result: {error}"))
                    })?;
            }
            PolicyDecision::Forbid { .. } => {
                self.store
                    .finish_tool_call(
                        frame.id.clone(),
                        intent.id,
                        intent.revision,
                        Err(policy_denied()),
                    )
                    .await
                    .map_err(|error| {
                        StoreError::InvalidData(format!("persist denied tool result: {error}"))
                    })?;
            }
            PolicyDecision::NeedsPermission => {
                self.store
                    .create_permission_request(
                        frame.id.clone(),
                        intent.revision,
                        NewPermissionRequest {
                            id: PermissionRequestId::new(uuid::Uuid::now_v7()),
                            provider_call_id,
                            qualified_tool: binding.qualified_name,
                            descriptor_digest: binding.descriptor_digest,
                            input_digest: canonical_sha256(&arguments)
                                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
                            policy_revision,
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

fn policy_denied() -> Value {
    json!({"code":"policy_denied","message":"Tool invocation denied by session policy"})
}

fn active_execution_limit_millis(definition: &crate::dsl::CompiledDefinition) -> u64 {
    definition
        .limits
        .get("active_execution_seconds")
        .copied()
        .unwrap_or(u64::MAX)
        .saturating_mul(1_000)
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

fn model_tool(binding: crate::RunToolBinding) -> Value {
    json!({"type":"function", "name": binding.qualified_name, "description": binding.descriptor.get("description").and_then(Value::as_str).unwrap_or(""), "parameters": binding.descriptor.get("input").cloned().unwrap_or_else(|| json!({"type":"object","properties":{},"additionalProperties":false}))})
}

fn tool_call_from_item(item: &Value) -> Option<(String, String, Value)> {
    if item.get("type")?.as_str()? != "function_call"
        || item.get("name")?.as_str()? == "complete_task"
    {
        return None;
    }
    let arguments = item.get("arguments")?;
    let arguments = arguments
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| arguments.clone());
    Some((
        item.get("call_id")
            .or_else(|| item.get("id"))?
            .as_str()?
            .to_owned(),
        item.get("name")?.as_str()?.to_owned(),
        arguments,
    ))
}

enum ModelAction {
    Completion(String, Value),
    ToolCall(String, String, Value),
}

fn action_from_item(item: &Value) -> Option<ModelAction> {
    parse_completion_item(item)
        .map(|(outcome, fields)| ModelAction::Completion(outcome, fields))
        .or_else(|| {
            tool_call_from_item(item)
                .map(|(id, name, arguments)| ModelAction::ToolCall(id, name, arguments))
        })
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
            super::tool_call_from_item(
                &json!({"type":"function_call","call_id":"c1","name":"docs/search","arguments":"{\"query\":\"x\"}"})
            ),
            Some((
                "c1".to_owned(),
                "docs/search".to_owned(),
                json!({"query":"x"})
            )),
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
