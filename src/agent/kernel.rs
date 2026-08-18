use std::sync::Arc;

use crate::{
    AcceptedUserTask, AgentCommandState, AgentExecutionView, AgentRecoveryReport, AgentRuntime,
    CancelUserTask, CompleteAgentCommand, FailAgentCommand, GoalPhase, KernelCommand,
    KernelCommandPayload, OperationalStatus, PauseUserTask, PolicyRevision, PostgresStore,
    PrincipalId, ResumeUserTask, RuntimeCeilings, RuntimeEvent, RuntimeHandle, RuntimeStart,
    StartAgent, StoreError, TaskLifecycle, validate_transition,
};

use super::principal_resumed_event_id;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentLifecycleAction {
    Start,
    CompleteGoal,
    BlockGoal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentPolicyDecision {
    pub allowed: bool,
    pub policy_revision: PolicyRevision,
    pub reason_code: String,
}

#[async_trait::async_trait]
pub trait AgentLifecycleAuthorizer: Send + Sync {
    async fn authorize(
        &self,
        principal: &PrincipalId,
        task: &AcceptedUserTask,
        action: AgentLifecycleAction,
    ) -> Result<AgentPolicyDecision, StoreError>;
}

pub struct TaskOwnerAuthorizer {
    store: Arc<PostgresStore>,
}

impl TaskOwnerAuthorizer {
    #[must_use]
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl AgentLifecycleAuthorizer for TaskOwnerAuthorizer {
    async fn authorize(
        &self,
        principal: &PrincipalId,
        task: &AcceptedUserTask,
        _action: AgentLifecycleAction,
    ) -> Result<AgentPolicyDecision, StoreError> {
        Ok(AgentPolicyDecision {
            allowed: principal == &task.principal,
            policy_revision: self.store.policy_revision(task.session_id.clone()).await?,
            reason_code: "authenticated_task_owner".to_owned(),
        })
    }
}

pub struct AgentKernel {
    store: Arc<PostgresStore>,
    runtime: Arc<dyn AgentRuntime>,
    authorizer: Arc<dyn AgentLifecycleAuthorizer>,
    ceilings: RuntimeCeilings,
    #[cfg(feature = "test-support")]
    completion_probe_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl AgentKernel {
    #[must_use]
    pub fn new(
        store: Arc<PostgresStore>,
        runtime: Arc<dyn AgentRuntime>,
        authorizer: Arc<dyn AgentLifecycleAuthorizer>,
    ) -> Self {
        let ceilings = *store.agent_runtime_ceilings();
        Self {
            store,
            runtime,
            authorizer,
            ceilings,
            #[cfg(feature = "test-support")]
            completion_probe_hook: None,
        }
    }

    #[cfg(feature = "test-support")]
    // This hook exists only for deterministic concurrency tests. Production
    // builds do not compile the field, setter, or call site.
    #[doc(hidden)]
    pub fn set_completion_probe_hook<F>(&mut self, hook: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.completion_probe_hook = Some(Arc::new(hook));
    }

    pub(crate) fn store(&self) -> &PostgresStore {
        &self.store
    }

    pub async fn start_agent(
        &self,
        principal: &PrincipalId,
        request: StartAgent,
    ) -> Result<AgentExecutionView, StoreError> {
        if let Some(replay) = self.store.replay_agent_start(principal, &request).await? {
            return Ok(replay);
        }
        let seed = self
            .store
            .load_agent_start_seed(request.session_id.clone(), request.task_id.clone())
            .await?;
        if seed.execution.runtime.is_some() {
            if let Some(replay) = self.store.replay_agent_start(principal, &request).await? {
                return Ok(replay);
            }
            return Err(StoreError::AgentTaskAlreadyTerminal);
        }
        self.runtime
            .validate_agent(&seed.definition)
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        let policy = self
            .authorizer
            .authorize(principal, &seed.execution.task, AgentLifecycleAction::Start)
            .await?;
        if !policy.allowed {
            return Err(StoreError::AgentTransitionInvalid(
                "current policy denied agent start".into(),
            ));
        }
        let transition = self
            .runtime
            .start(RuntimeStart {
                execution: &seed.execution,
                definition: &seed.definition,
            })
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        validate_transition(&transition, &seed.execution, &self.ceilings)
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        self.store
            .commit_agent_start(crate::AgentStartCommit {
                principal,
                request: &request,
                frozen_definition_id: seed.execution.task.definition.id,
                frozen_definition_hash: &seed.execution.task.definition.definition_hash,
                policy: &policy,
                transition: &transition,
                ceilings: &self.ceilings,
            })
            .await
    }

    pub async fn complete_command(
        &self,
        principal: &PrincipalId,
        request: CompleteAgentCommand,
    ) -> Result<AgentExecutionView, StoreError> {
        self.complete_command_with_shape(principal, request, false)
            .await
    }

    pub async fn fail_command(
        &self,
        principal: &PrincipalId,
        request: FailAgentCommand,
    ) -> Result<AgentExecutionView, StoreError> {
        if request.code.is_empty() || request.code.len() > 128 {
            return Err(StoreError::AgentTransitionInvalid(
                "command failure code is invalid".to_owned(),
            ));
        }
        let context = self
            .store
            .load_agent_command_context(
                request.session_id.clone(),
                request.task_id.clone(),
                request.command_id.clone(),
            )
            .await?;
        let completion = CompleteAgentCommand {
            session_id: request.session_id,
            task_id: request.task_id,
            command_id: request.command_id.clone(),
            expected_command_revision: request.expected_command_revision,
            idempotency_key: request.idempotency_key.clone(),
            event: RuntimeEvent::CommandFailed {
                event_id: format!(
                    "command-failed:{}:{}",
                    request.command_id.as_uuid(),
                    request.idempotency_key.as_str()
                ),
                command_key: context.command.key,
                code: request.code,
            },
        };
        self.complete_command_with_shape(principal, completion, true)
            .await
    }

    async fn complete_command_with_shape(
        &self,
        principal: &PrincipalId,
        request: CompleteAgentCommand,
        require_fail_task: bool,
    ) -> Result<AgentExecutionView, StoreError> {
        if let Some(replay) = self
            .store
            .replay_agent_command_completion(principal, &request)
            .await?
        {
            return Ok(replay);
        }
        #[cfg(feature = "test-support")]
        if let Some(hook) = &self.completion_probe_hook {
            hook();
        }
        let context = self
            .store
            .load_agent_command_context(
                request.session_id.clone(),
                request.task_id.clone(),
                request.command_id.clone(),
            )
            .await?;
        let stale_context_error = if context.execution.goal_phase == GoalPhase::Complete {
            Some(StoreError::GoalAlreadyTerminal)
        } else if context.execution.lifecycle != TaskLifecycle::Running
            || context.execution.operational_status == OperationalStatus::Finished
        {
            Some(StoreError::AgentTaskAlreadyTerminal)
        } else if context.command.state != AgentCommandState::Claimed {
            Some(StoreError::RevisionConflict)
        } else {
            None
        };
        if let Some(error) = stale_context_error {
            // The initial read-only receipt probe can race a winning completion.
            // Once context exposes terminal or consumed state, the winner is
            // committed and its receipt is visible, so resolve exact replay or
            // conflict before returning the context-derived error.
            if let Some(replay) = self
                .store
                .replay_agent_command_completion(principal, &request)
                .await?
            {
                return Ok(replay);
            }
            return Err(error);
        }
        if request.event.command_key() != Some(&context.command.key) {
            return Err(StoreError::AgentTransitionInvalid(
                "event command key does not match persisted command".to_owned(),
            ));
        }
        let transition = self
            .runtime
            .handle(RuntimeHandle {
                execution: &context.execution,
                definition: &context.definition,
                event: &request.event,
            })
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        validate_transition(&transition, &context.execution, &self.ceilings)
            .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
        if require_fail_task
            && !matches!(
                transition.commands.as_slice(),
                [KernelCommand {
                    payload: KernelCommandPayload::FailTask(_),
                    ..
                }]
            )
        {
            return Err(StoreError::AgentTransitionInvalid(
                "command failure must produce exactly one FailTask".to_owned(),
            ));
        }
        let policy = match transition.commands.as_slice() {
            [
                KernelCommand {
                    payload: KernelCommandPayload::CompleteGoal(_),
                    ..
                },
            ] => Some(
                self.authorizer
                    .authorize(
                        principal,
                        &context.execution.task,
                        AgentLifecycleAction::CompleteGoal,
                    )
                    .await?,
            ),
            [
                KernelCommand {
                    payload: KernelCommandPayload::BlockGoal(_),
                    ..
                },
            ] => Some(
                self.authorizer
                    .authorize(
                        principal,
                        &context.execution.task,
                        AgentLifecycleAction::BlockGoal,
                    )
                    .await?,
            ),
            _ => None,
        };
        if policy.as_ref().is_some_and(|decision| !decision.allowed) {
            return Err(StoreError::AgentTransitionInvalid(
                "current policy denied goal transition".to_owned(),
            ));
        }
        self.store
            .commit_agent_command_transition(
                principal,
                &request,
                &transition,
                policy.as_ref(),
                &self.ceilings,
            )
            .await
    }

    pub async fn recover(&self, limit: i64) -> Result<AgentRecoveryReport, StoreError> {
        self.store.recover_agent_commands(limit).await
    }

    pub async fn pause_task(
        &self,
        principal: &PrincipalId,
        request: PauseUserTask,
    ) -> Result<AgentExecutionView, StoreError> {
        self.store.pause_user_task(principal, request).await
    }

    pub async fn resume_task(
        &self,
        principal: &PrincipalId,
        request: ResumeUserTask,
    ) -> Result<AgentExecutionView, StoreError> {
        self.store
            .authenticate_agent_task_owner(principal, &request.session_id, &request.task_id)
            .await?;
        let execution = self
            .store
            .get_agent_execution(request.session_id.clone(), request.task_id.clone())
            .await?;
        let transition = if execution.goal_phase == GoalPhase::Blocked {
            let seed = self
                .store
                .load_agent_start_seed(request.session_id.clone(), request.task_id.clone())
                .await?;
            let event = RuntimeEvent::PrincipalResumed {
                event_id: principal_resumed_event_id(&request.task_id, &request.idempotency_key),
            };
            let transition = self
                .runtime
                .handle(RuntimeHandle {
                    execution: &execution,
                    definition: &seed.definition,
                    event: &event,
                })
                .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
            validate_transition(&transition, &execution, &self.ceilings)
                .map_err(|error| StoreError::AgentTransitionInvalid(error.to_string()))?;
            Some(transition)
        } else {
            None
        };
        self.store
            .resume_user_task(principal, request, transition.as_ref(), &self.ceilings)
            .await
    }

    pub async fn cancel_task(
        &self,
        principal: &PrincipalId,
        request: CancelUserTask,
    ) -> Result<AgentExecutionView, StoreError> {
        self.store.cancel_user_task(principal, request).await
    }
}
