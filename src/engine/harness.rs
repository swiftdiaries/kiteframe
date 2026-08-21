use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::harness::{
    HarnessApprovalBroker, HarnessFailure, HarnessInvocationId, HarnessInvocationRequest,
    HarnessProfileId, HarnessRegistry, HarnessRouteTable, HarnessTerminal, HarnessTerminalJournal,
    HarnessTransport, PostgresHarnessEventSink, canonical_outcome_unknown_terminal_payload,
    dispatch_with_terminal_journal, postgres_harness_acceptance_barrier, terminal_idempotency_key,
};
use crate::store::{HarnessInvocationState, HarnessInvocationView, PrepareHarnessInvocation};
use crate::{
    AgentCommandState, AgentKernel, ClaimAgentCommand, CompleteAgentCommand, EvidenceId,
    FailAgentCommand, GoalPhase, IdempotencyKey, KernelCommandPayload, OperationalStatus,
    PrincipalId, RuntimeEvent, SessionId, StoreError, TaskLifecycle,
};

const HARNESS_DISPATCHER_CLAIMANT: &str = "harness-dispatcher-v1";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// One bounded recovery page. The three counters are mutually exclusive and
/// their sum never exceeds the requested positive limit. A dispatcher does
/// not recount pre-accept rows that it already returned on an earlier page.
pub struct HarnessRecoveryReport {
    pub retryable_pre_accept: u64,
    pub outcome_unknown: u64,
    pub terminal: u64,
}

pub struct AgentCommandDispatcher {
    kernel: Arc<AgentKernel>,
    registry: Arc<HarnessRegistry>,
    approvals: Arc<dyn HarnessApprovalBroker>,
    routes: HarnessRouteTable,
    active: Arc<Mutex<BTreeSet<HarnessInvocationId>>>,
    recovery_seen: Arc<Mutex<BTreeSet<HarnessInvocationId>>>,
    recovery_lock: tokio::sync::Mutex<()>,
}

impl AgentKernel {
    #[must_use]
    pub fn with_harness_registry(
        self: Arc<Self>,
        registry: Arc<HarnessRegistry>,
        approvals: Arc<dyn HarnessApprovalBroker>,
    ) -> AgentCommandDispatcher {
        let builtin = registry
            .sole_profile_id_for_transport(HarnessTransport::Builtin)
            .expect("one built-in harness profile must be registered");
        self.with_harness_registry_routes(
            registry,
            approvals,
            HarnessRouteTable::new(builtin.clone(), builtin),
        )
    }

    #[must_use]
    pub fn with_harness_registry_routes(
        self: Arc<Self>,
        registry: Arc<HarnessRegistry>,
        approvals: Arc<dyn HarnessApprovalBroker>,
        routes: HarnessRouteTable,
    ) -> AgentCommandDispatcher {
        AgentCommandDispatcher {
            kernel: self,
            registry,
            approvals,
            routes,
            active: Arc::new(Mutex::new(BTreeSet::new())),
            recovery_seen: Arc::new(Mutex::new(BTreeSet::new())),
            recovery_lock: tokio::sync::Mutex::new(()),
        }
    }
}

impl AgentCommandDispatcher {
    pub async fn recover_harness_invocations(
        &self,
        limit: i64,
    ) -> Result<HarnessRecoveryReport, StoreError> {
        if limit <= 0 {
            return Ok(HarnessRecoveryReport::default());
        }
        let _recovery = self.recovery_lock.lock().await;
        let excluded = self
            .recovery_seen
            .lock()
            .expect("dispatcher recovery lock is not poisoned")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let recovered = self
            .kernel
            .store()
            .recover_harness_invocations_excluding(limit, &excluded)
            .await?;
        let mut seen = BTreeSet::new();
        let mut report = HarnessRecoveryReport::default();
        for candidate in recovered {
            let invocation = if candidate.terminal {
                candidate
            } else {
                let Some(invocation) = self
                    .kernel
                    .store()
                    .refresh_harness_recovery_invocation(&candidate.invocation_id)
                    .await?
                else {
                    self.recovery_seen
                        .lock()
                        .expect("dispatcher recovery lock is not poisoned")
                        .insert(candidate.invocation_id);
                    continue;
                };
                invocation
            };
            if !seen.insert(invocation.invocation_id.clone()) {
                continue;
            }
            match invocation.state {
                HarnessInvocationState::Prepared | HarnessInvocationState::Started => {
                    report.retryable_pre_accept += 1;
                }
                HarnessInvocationState::Accepted | HarnessInvocationState::OutcomeUnknown => {
                    report.outcome_unknown += 1;
                    let context = self.recovered_command_context(&invocation).await?;
                    self.settle_outcome_unknown(
                        &context.execution,
                        &context.command,
                        &invocation.invocation_id,
                        invocation.attempt,
                    )
                    .await?;
                }
                HarnessInvocationState::Completed
                | HarnessInvocationState::Cancelled
                | HarnessInvocationState::Failed => {
                    report.terminal += 1;
                    let context = self.recovered_command_context(&invocation).await?;
                    if context.command.state == AgentCommandState::Claimed {
                        self.settle_harness_terminal(
                            &context.execution,
                            &context.command,
                            &invocation,
                            terminal_from_record(&invocation)?,
                        )
                        .await?;
                    }
                }
            }
            self.recovery_seen
                .lock()
                .expect("dispatcher recovery lock is not poisoned")
                .insert(invocation.invocation_id);
        }
        Ok(report)
    }

    async fn recovered_command_context(
        &self,
        invocation: &HarnessInvocationView,
    ) -> Result<crate::AgentCommandContext, StoreError> {
        let (session_id, task_id): (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
            "SELECT tasks.session_id, commands.task_id FROM agent_commands commands \
             JOIN user_tasks tasks ON tasks.id = commands.task_id \
             WHERE commands.id = $1",
        )
        .bind(invocation.command_id.as_uuid())
        .fetch_one(self.kernel.store().pool())
        .await?;
        self.kernel
            .store()
            .load_agent_command_context(
                SessionId::new(session_id),
                crate::UserTaskId::new(task_id),
                invocation.command_id.clone(),
            )
            .await
    }

    pub async fn tick_agent_commands(&self, limit: i64) -> Result<usize, StoreError> {
        if limit <= 0 {
            return Ok(0);
        }
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
                        claimant: HARNESS_DISPATCHER_CLAIMANT.to_owned(),
                        idempotency_key: idempotency_key(format!(
                            "{HARNESS_DISPATCHER_CLAIMANT}:{}",
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
                    if listed.claimant.as_deref() == Some(HARNESS_DISPATCHER_CLAIMANT) =>
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
            DispatchRoute::Harness => self.dispatch_harness(execution, claimed).await,
            DispatchRoute::Unavailable => {
                let failure = failure_request(
                    execution,
                    &claimed,
                    &format!("harness-unavailable:{}", claimed.id.as_uuid()),
                    "runtime_command_unavailable",
                )?;
                unavailable_or_audit_late(&self.kernel, &execution.task.principal, &failure).await
            }
            DispatchRoute::InternalAuthority => Err(StoreError::AgentTransitionInvalid(
                "internal authority command reached dispatcher".to_owned(),
            )),
        }
    }

    async fn dispatch_harness(
        &self,
        execution: &crate::AgentExecutionView,
        claimed: crate::AgentCommandView,
    ) -> Result<(), StoreError> {
        let KernelCommandPayload::InvokeHarness(payload) = &claimed.payload else {
            unreachable!("harness route must contain InvokeHarness")
        };
        let prepared = if let Some(stored) = self
            .kernel
            .store()
            .harness_invocation_for_command_revision(&claimed.id, claimed.revision)
            .await?
        {
            if stored.payload_digest != claimed.payload_digest {
                return Err(StoreError::IdempotencyConflict);
            }
            stored
        } else {
            let profile_id = self
                .routes
                .select(execution.task.preferences.harness_role, payload.role)
                .clone();
            let profile_digest = self
                .registry
                .profile_digest(&profile_id)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?;
            let candidate = HarnessInvocationId::new(uuid::Uuid::now_v7());
            self.kernel
                .store()
                .prepare_harness_invocation(PrepareHarnessInvocation {
                    invocation_id: &candidate,
                    command_id: &claimed.id,
                    command_revision: claimed.revision,
                    profile_id: &profile_id,
                    profile_digest: &profile_digest,
                    payload_digest: &claimed.payload_digest,
                })
                .await?
        };
        let profile_id = prepared.profile_id.clone();
        let journal = PreparedHarnessTerminalJournal::new(&prepared);
        dispatch_with_terminal_journal(
            &journal,
            &prepared.invocation_id,
            |terminal| async {
                self.settle_harness_terminal(execution, &claimed, &prepared, terminal)
                    .await
            },
            || async {
                let Some(_active) =
                    ActiveDispatchGuard::reserve(self.active.clone(), &prepared.invocation_id)
                else {
                    return Ok(());
                };
                let prepared = match prepared.state {
                    HarnessInvocationState::Prepared => {
                        self.kernel
                            .store()
                            .mark_harness_invocation_started(
                                &prepared.invocation_id,
                                prepared.attempt,
                            )
                            .await?
                    }
                    HarnessInvocationState::Started => prepared.clone(),
                    HarnessInvocationState::Accepted => {
                        return self
                            .settle_outcome_unknown(
                                execution,
                                &claimed,
                                &prepared.invocation_id,
                                prepared.attempt,
                            )
                            .await;
                    }
                    HarnessInvocationState::Completed
                    | HarnessInvocationState::Cancelled
                    | HarnessInvocationState::Failed
                    | HarnessInvocationState::OutcomeUnknown => return Ok(()),
                };
                if prepared.terminal {
                    return self
                        .settle_harness_terminal(
                            execution,
                            &claimed,
                            &prepared,
                            terminal_from_record(&prepared)?,
                        )
                        .await;
                }
                let workspace: String =
                    sqlx::query_scalar("SELECT working_directory FROM sessions WHERE id = $1")
                        .bind(execution.task.session_id.as_uuid())
                        .fetch_one(self.kernel.store().pool())
                        .await?;
                let request = HarnessInvocationRequest {
                    invocation_id: prepared.invocation_id.clone(),
                    command_id: claimed.id.clone(),
                    command_revision: claimed.revision,
                    profile_id,
                    workspace: PathBuf::from(workspace),
                    payload: payload.clone(),
                    payload_digest: claimed.payload_digest.clone(),
                };
                let next_sequence = u64::try_from(
                    self.kernel
                        .store()
                        .harness_invocation_events(&prepared.invocation_id)
                        .await?
                        .len(),
                )
                .map_err(|_| {
                    StoreError::InvalidData("harness event sequence is malformed".to_owned())
                })?;
                let store = self.kernel.store_arc();
                let invocation = self.registry.invoke_from_sequence_with_acceptance_barrier(
                    request,
                    self.approvals.clone(),
                    Arc::new(PostgresHarnessEventSink::new(store.clone())),
                    next_sequence,
                    postgres_harness_acceptance_barrier(
                        store,
                        prepared.invocation_id.clone(),
                        prepared.attempt,
                    ),
                );
                tokio::pin!(invocation);
                let cancellation = self.wait_for_command_cancellation(&claimed.id);
                tokio::pin!(cancellation);
                let terminal = tokio::select! {
                    biased;
                    terminal = &mut invocation => terminal,
                    cancelled = &mut cancellation => {
                        cancelled?;
                        let _ = self.registry.cancel(&prepared.invocation_id).await;
                        invocation.await
                    }
                };
                self.settle_harness_terminal(execution, &claimed, &prepared, terminal)
                    .await
            },
        )
        .await
    }

    async fn settle_harness_terminal(
        &self,
        execution: &crate::AgentExecutionView,
        claimed: &crate::AgentCommandView,
        invocation: &crate::store::HarnessInvocationView,
        terminal: HarnessTerminal,
    ) -> Result<(), StoreError> {
        if self.command_is_cancelled(&claimed.id).await? {
            return self
                .settle_late_terminal_after_cancel(execution, claimed, invocation, terminal)
                .await;
        }
        let terminal_key = terminal_idempotency_key(&invocation.invocation_id);
        let terminal_payload = terminal_payload(&terminal);
        match terminal {
            HarnessTerminal::Completed(output) => {
                let completion = CompleteAgentCommand {
                    session_id: execution.task.session_id.clone(),
                    task_id: claimed.task_id.clone(),
                    command_id: claimed.id.clone(),
                    expected_command_revision: claimed.revision,
                    idempotency_key: idempotency_key(format!(
                        "harness-result:{}",
                        invocation.invocation_id.as_uuid()
                    ))?,
                    event: RuntimeEvent::HarnessResult {
                        event_id: format!("harness-result:{}", invocation.invocation_id.as_uuid()),
                        command_key: claimed.key.clone(),
                        outcome: output.outcome,
                        fields: output.fields,
                        evidence_id: EvidenceId::new(invocation.invocation_id.as_uuid()),
                        history: output.history,
                    },
                };
                complete_or_audit_late(&self.kernel, &execution.task.principal, &completion)
                    .await?;
                self.kernel
                    .store()
                    .settle_harness_invocation(
                        &invocation.invocation_id,
                        invocation.attempt,
                        HarnessInvocationState::Completed,
                        &terminal_key,
                        &terminal_payload,
                    )
                    .await?;
                Ok(())
            }
            HarnessTerminal::Cancelled => {
                let failure =
                    failure_request(execution, claimed, &terminal_key, "harness_cancelled")?;
                failure_or_audit_late(&self.kernel, &execution.task.principal, &failure).await?;
                self.kernel
                    .store()
                    .settle_harness_invocation(
                        &invocation.invocation_id,
                        invocation.attempt,
                        HarnessInvocationState::Cancelled,
                        &terminal_key,
                        &terminal_payload,
                    )
                    .await?;
                Ok(())
            }
            HarnessTerminal::Retryable(_) => {
                let max_pre_accept_attempts =
                    registered_pre_accept_attempt_limit(&self.registry, &invocation.profile_id)?;
                let retry = self
                    .kernel
                    .store()
                    .record_harness_retryable_failure(
                        &invocation.invocation_id,
                        invocation.attempt,
                        max_pre_accept_attempts,
                        &terminal_key,
                        &terminal_payload,
                    )
                    .await?;
                if retry.terminal {
                    let code = if retry.state == HarnessInvocationState::Cancelled {
                        "harness_cancelled"
                    } else {
                        "harness_failed"
                    };
                    let failure = failure_request(execution, claimed, &terminal_key, code)?;
                    failure_or_audit_late(&self.kernel, &execution.task.principal, &failure)
                        .await?;
                }
                Ok(())
            }
            HarnessTerminal::Failed(failure) => {
                let transport = self
                    .kernel
                    .store()
                    .harness_plugin_transport(&invocation.profile_id)
                    .await?;
                if is_builtin_command_unavailable(transport, &failure) {
                    let failure = failure_request(
                        execution,
                        claimed,
                        &terminal_key,
                        "runtime_command_unavailable",
                    )?;
                    unavailable_or_audit_late(&self.kernel, &execution.task.principal, &failure)
                        .await?;
                } else {
                    let failure = failure_request(execution, claimed, &terminal_key, failure.code)?;
                    failure_or_audit_late(&self.kernel, &execution.task.principal, &failure)
                        .await?;
                }
                self.kernel
                    .store()
                    .settle_harness_invocation(
                        &invocation.invocation_id,
                        invocation.attempt,
                        HarnessInvocationState::Failed,
                        &terminal_key,
                        &terminal_payload,
                    )
                    .await?;
                Ok(())
            }
            HarnessTerminal::OutcomeUnknown(_) => {
                self.settle_outcome_unknown(
                    execution,
                    claimed,
                    &invocation.invocation_id,
                    invocation.attempt,
                )
                .await
            }
        }
    }

    async fn wait_for_command_cancellation(
        &self,
        command_id: &crate::AgentCommandId,
    ) -> Result<(), StoreError> {
        loop {
            if self.command_is_cancelled(command_id).await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn command_is_cancelled(
        &self,
        command_id: &crate::AgentCommandId,
    ) -> Result<bool, StoreError> {
        let state: String = sqlx::query_scalar("SELECT state FROM agent_commands WHERE id = $1")
            .bind(command_id.as_uuid())
            .fetch_one(self.kernel.store().pool())
            .await?;
        Ok(state == "cancelled")
    }

    async fn settle_late_terminal_after_cancel(
        &self,
        execution: &crate::AgentExecutionView,
        claimed: &crate::AgentCommandView,
        invocation: &HarnessInvocationView,
        terminal: HarnessTerminal,
    ) -> Result<(), StoreError> {
        let terminal_key = terminal_idempotency_key(&invocation.invocation_id);
        if !invocation.terminal {
            self.kernel
                .store()
                .settle_harness_invocation(
                    &invocation.invocation_id,
                    invocation.attempt,
                    HarnessInvocationState::Cancelled,
                    &terminal_key,
                    &serde_json::json!({"type": "cancelled"}),
                )
                .await?;
        }
        match terminal {
            HarnessTerminal::Completed(output) => {
                let completion = CompleteAgentCommand {
                    session_id: execution.task.session_id.clone(),
                    task_id: claimed.task_id.clone(),
                    command_id: claimed.id.clone(),
                    expected_command_revision: claimed.revision,
                    idempotency_key: idempotency_key(format!(
                        "harness-result:{}",
                        invocation.invocation_id.as_uuid()
                    ))?,
                    event: RuntimeEvent::HarnessResult {
                        event_id: format!("harness-result:{}", invocation.invocation_id.as_uuid()),
                        command_key: claimed.key.clone(),
                        outcome: output.outcome,
                        fields: output.fields,
                        evidence_id: EvidenceId::new(invocation.invocation_id.as_uuid()),
                        history: output.history,
                    },
                };
                self.kernel
                    .store()
                    .audit_late_agent_command_completion(&execution.task.principal, &completion)
                    .await
            }
            HarnessTerminal::Cancelled => {
                let failure =
                    failure_request(execution, claimed, &terminal_key, "harness_cancelled")?;
                self.kernel
                    .store()
                    .audit_late_agent_command_failure(&execution.task.principal, &failure)
                    .await
            }
            HarnessTerminal::Retryable(failure)
            | HarnessTerminal::Failed(failure)
            | HarnessTerminal::OutcomeUnknown(failure) => {
                let transport = self
                    .kernel
                    .store()
                    .harness_plugin_transport(&invocation.profile_id)
                    .await?;
                let code = if is_builtin_command_unavailable(transport, &failure) {
                    "runtime_command_unavailable"
                } else {
                    failure.code
                };
                let failure = failure_request(execution, claimed, &terminal_key, code)?;
                self.kernel
                    .store()
                    .audit_late_agent_command_failure(&execution.task.principal, &failure)
                    .await
            }
        }
    }

    async fn settle_outcome_unknown(
        &self,
        execution: &crate::AgentExecutionView,
        claimed: &crate::AgentCommandView,
        invocation_id: &HarnessInvocationId,
        attempt: u64,
    ) -> Result<(), StoreError> {
        let terminal_key = terminal_idempotency_key(invocation_id);
        let failure =
            failure_request(execution, claimed, &terminal_key, "harness_outcome_unknown")?;
        failure_or_audit_late(&self.kernel, &execution.task.principal, &failure).await?;
        self.kernel
            .store()
            .settle_harness_invocation(
                invocation_id,
                attempt,
                HarnessInvocationState::OutcomeUnknown,
                &terminal_key,
                &canonical_outcome_unknown_terminal_payload(),
            )
            .await?;
        Ok(())
    }
}

fn registered_pre_accept_attempt_limit(
    registry: &HarnessRegistry,
    profile_id: &HarnessProfileId,
) -> Result<u64, StoreError> {
    registry
        .pre_accept_attempt_limit(profile_id)
        .map_err(|error| StoreError::InvalidData(error.to_string()))
}

fn is_builtin_command_unavailable(transport: HarnessTransport, failure: &HarnessFailure) -> bool {
    transport == HarnessTransport::Builtin && failure.code == "harness_capability_unsupported"
}

struct PreparedHarnessTerminalJournal<'a> {
    invocation: &'a HarnessInvocationView,
}

impl<'a> PreparedHarnessTerminalJournal<'a> {
    fn new(invocation: &'a HarnessInvocationView) -> Self {
        Self { invocation }
    }
}

#[async_trait::async_trait]
impl HarnessTerminalJournal for PreparedHarnessTerminalJournal<'_> {
    type Error = StoreError;

    async fn terminal(
        &self,
        invocation_id: &HarnessInvocationId,
    ) -> Result<Option<HarnessTerminal>, Self::Error> {
        if self.invocation.invocation_id != *invocation_id {
            return Err(StoreError::InvalidData(
                "terminal journal invocation identity drifted".to_owned(),
            ));
        }
        self.invocation
            .terminal
            .then(|| terminal_from_record(self.invocation))
            .transpose()
    }
}

struct ActiveDispatchGuard {
    active: Arc<Mutex<BTreeSet<HarnessInvocationId>>>,
    invocation_id: HarnessInvocationId,
}

impl ActiveDispatchGuard {
    fn reserve(
        active: Arc<Mutex<BTreeSet<HarnessInvocationId>>>,
        invocation_id: &HarnessInvocationId,
    ) -> Option<Self> {
        let mut entries = active
            .lock()
            .expect("dispatcher active lock is not poisoned");
        if !entries.insert(invocation_id.clone()) {
            return None;
        }
        drop(entries);
        Some(Self {
            active,
            invocation_id: invocation_id.clone(),
        })
    }
}

impl Drop for ActiveDispatchGuard {
    fn drop(&mut self) {
        self.active
            .lock()
            .expect("dispatcher active lock is not poisoned")
            .remove(&self.invocation_id);
    }
}

fn terminal_payload(terminal: &HarnessTerminal) -> serde_json::Value {
    match terminal {
        HarnessTerminal::Completed(output) => serde_json::json!({
            "type": "completed",
            "outcome": output.outcome,
            "fields": output.fields,
            "history": output.history,
        }),
        HarnessTerminal::Cancelled => serde_json::json!({"type": "cancelled"}),
        HarnessTerminal::Retryable(failure) | HarnessTerminal::Failed(failure) => {
            serde_json::json!({"type": "failed", "code": failure.code, "detail": failure.detail})
        }
        HarnessTerminal::OutcomeUnknown(failure) => serde_json::json!({
            "type": "outcome_unknown",
            "code": failure.code,
            "detail": failure.detail,
        }),
    }
}

fn terminal_from_record(
    invocation: &crate::store::HarnessInvocationView,
) -> Result<HarnessTerminal, StoreError> {
    let payload = invocation.terminal_payload.as_ref().ok_or_else(|| {
        StoreError::InvalidData("terminal harness invocation payload is absent".to_owned())
    })?;
    let kind = payload
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            StoreError::InvalidData("terminal harness invocation payload is malformed".to_owned())
        })?;
    match kind {
        "completed" if invocation.state == HarnessInvocationState::Completed => Ok(
            HarnessTerminal::Completed(crate::harness::HarnessInvocationOutput {
                outcome: payload
                    .get("outcome")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        StoreError::InvalidData(
                            "terminal completed outcome is malformed".to_owned(),
                        )
                    })?
                    .to_owned(),
                fields: payload.get("fields").cloned().ok_or_else(|| {
                    StoreError::InvalidData("terminal completed fields are malformed".to_owned())
                })?,
                history: serde_json::from_value(payload.get("history").cloned().ok_or_else(
                    || {
                        StoreError::InvalidData(
                            "terminal completed history is malformed".to_owned(),
                        )
                    },
                )?)
                .map_err(|error| StoreError::InvalidData(error.to_string()))?,
            }),
        ),
        "cancelled" if invocation.state == HarnessInvocationState::Cancelled => {
            Ok(HarnessTerminal::Cancelled)
        }
        "failed" if invocation.state == HarnessInvocationState::Failed => {
            Ok(HarnessTerminal::Failed(HarnessFailure::new(
                terminal_code(payload)?,
                terminal_detail(payload)?,
            )))
        }
        "outcome_unknown" if invocation.state == HarnessInvocationState::OutcomeUnknown => {
            Ok(HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
                terminal_code(payload)?,
                terminal_detail(payload)?,
            )))
        }
        _ => Err(StoreError::InvalidData(
            "terminal harness invocation payload conflicts with its state".to_owned(),
        )),
    }
}

fn terminal_code(payload: &serde_json::Value) -> Result<&'static str, StoreError> {
    match payload.get("code").and_then(serde_json::Value::as_str) {
        Some("harness_profile_unavailable") => Ok("harness_profile_unavailable"),
        Some("harness_protocol_incompatible") => Ok("harness_protocol_incompatible"),
        Some("harness_capability_unsupported") => Ok("harness_capability_unsupported"),
        Some("harness_permission_rejected") => Ok("harness_permission_rejected"),
        Some("harness_cancelled") => Ok("harness_cancelled"),
        Some("harness_failed") => Ok("harness_failed"),
        Some("harness_outcome_unknown") => Ok("harness_outcome_unknown"),
        _ => Err(StoreError::InvalidData(
            "terminal harness failure code is malformed".to_owned(),
        )),
    }
}

fn terminal_detail(payload: &serde_json::Value) -> Result<&str, StoreError> {
    payload
        .get("detail")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            StoreError::InvalidData("terminal harness failure detail is malformed".to_owned())
        })
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
        idempotency_key: idempotency_key(prefix.to_owned())?,
        code: code.to_owned(),
    })
}

fn idempotency_key(value: String) -> Result<IdempotencyKey, StoreError> {
    IdempotencyKey::new(value).map_err(|error| StoreError::InvalidData(error.to_string()))
}

fn is_late_terminal(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::AgentTaskAlreadyTerminal | StoreError::GoalAlreadyTerminal
    )
}

async fn complete_or_audit_late(
    kernel: &AgentKernel,
    principal: &PrincipalId,
    completion: &CompleteAgentCommand,
) -> Result<(), StoreError> {
    match kernel.complete_command(principal, completion.clone()).await {
        Ok(_) => Ok(()),
        Err(error) if is_late_terminal(&error) => {
            kernel
                .store()
                .audit_late_agent_command_completion(principal, completion)
                .await
        }
        Err(error) => Err(error),
    }
}

async fn unavailable_or_audit_late(
    kernel: &AgentKernel,
    principal: &PrincipalId,
    failure: &FailAgentCommand,
) -> Result<(), StoreError> {
    match kernel
        .store()
        .fail_unavailable_agent_command(principal, failure)
        .await
    {
        Ok(_) => Ok(()),
        Err(error) if is_late_terminal(&error) => {
            kernel
                .store()
                .audit_late_agent_command_failure(principal, failure)
                .await
        }
        Err(error) => Err(error),
    }
}

async fn failure_or_audit_late(
    kernel: &AgentKernel,
    principal: &PrincipalId,
    failure: &FailAgentCommand,
) -> Result<(), StoreError> {
    match kernel.fail_command(principal, failure.clone()).await {
        Ok(_) => Ok(()),
        Err(error) if is_late_terminal(&error) => {
            kernel
                .store()
                .audit_late_agent_command_failure(principal, failure)
                .await
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use super::{
        ActiveDispatchGuard, DispatchRoute, dispatch_route, is_builtin_command_unavailable,
        terminal_from_record,
    };
    use crate::harness::{
        HarnessFailure, HarnessInvocationId, HarnessProfileId, HarnessTerminal, HarnessTransport,
    };
    use crate::store::{HarnessInvocationState, HarnessInvocationView};
    use crate::{AgentCommandId, KernelCommandPayload, RequestContext, WaitForEvent};
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

    #[test]
    fn only_builtin_capability_failure_restores_the_slice_two_unavailable_command_contract() {
        let unsupported = HarnessFailure::new("harness_capability_unsupported", "model_tool_calls");
        assert!(is_builtin_command_unavailable(
            HarnessTransport::Builtin,
            &unsupported,
        ));
        assert!(!is_builtin_command_unavailable(
            HarnessTransport::AcpStdio,
            &unsupported,
        ));
        assert!(!is_builtin_command_unavailable(
            HarnessTransport::Builtin,
            &HarnessFailure::new("harness_failed", "definitive failure"),
        ));
    }

    #[test]
    fn active_dispatch_guard_allows_exactly_one_concurrent_owner() {
        let active = Arc::new(Mutex::new(BTreeSet::new()));
        let invocation = HarnessInvocationId::new(uuid::Uuid::now_v7());
        let first = ActiveDispatchGuard::reserve(active.clone(), &invocation)
            .expect("first dispatcher owns the invocation");
        assert!(ActiveDispatchGuard::reserve(active.clone(), &invocation).is_none());
        drop(first);
        assert!(ActiveDispatchGuard::reserve(active, &invocation).is_some());
    }

    #[test]
    fn terminal_payload_reconstruction_preserves_completed_and_failed_kernel_inputs() {
        let invocation_id = HarnessInvocationId::new(uuid::Uuid::now_v7());
        let completed = terminal_from_record(&HarnessInvocationView {
            invocation_id: invocation_id.clone(),
            command_id: AgentCommandId::new(uuid::Uuid::now_v7()),
            command_revision: 0,
            profile_id: HarnessProfileId::new("builtin-v1").unwrap(),
            profile_digest: "sha256:profile".to_owned(),
            payload_digest: "sha256:payload".to_owned(),
            state: HarnessInvocationState::Completed,
            attempt: 0,
            prompt_accepted: true,
            terminal: true,
            terminal_request_digest: Some(format!("harness-terminal:{}", invocation_id.as_uuid())),
            terminal_payload: Some(json!({
                "type":"completed",
                "outcome":"ok",
                "fields":{"answer":"replayed"},
                "history":[],
            })),
        })
        .unwrap();
        assert!(matches!(completed, HarnessTerminal::Completed(output)
            if output.outcome == "ok" && output.fields == json!({"answer":"replayed"})));

        let failed = terminal_from_record(&HarnessInvocationView {
            invocation_id,
            command_id: AgentCommandId::new(uuid::Uuid::now_v7()),
            command_revision: 0,
            profile_id: HarnessProfileId::new("builtin-v1").unwrap(),
            profile_digest: "sha256:profile".to_owned(),
            payload_digest: "sha256:payload".to_owned(),
            state: HarnessInvocationState::Failed,
            attempt: 0,
            prompt_accepted: false,
            terminal: true,
            terminal_request_digest: Some("harness-terminal:test".to_owned()),
            terminal_payload: Some(json!({
                "type":"failed",
                "code":"harness_failed",
                "detail":"replayed failure",
            })),
        })
        .unwrap();
        assert!(matches!(failed, HarnessTerminal::Failed(failure)
            if failure.code == "harness_failed" && failure.detail == "replayed failure"));
    }
}
