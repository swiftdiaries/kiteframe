use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::harness::{
    HarnessApprovalBroker, HarnessFailure, HarnessInvocationId, HarnessInvocationRequest,
    HarnessProfileId, HarnessRegistry, HarnessRouteTable, HarnessTerminal, HarnessTerminalJournal,
    PostgresHarnessEventSink, canonical_outcome_unknown_terminal_payload,
    dispatch_with_terminal_journal, terminal_idempotency_key,
};
use crate::store::{HarnessInvocationState, HarnessInvocationView, PrepareHarnessInvocation};
use crate::{
    AgentCommandState, AgentKernel, ClaimAgentCommand, CompleteAgentCommand, EvidenceId,
    FailAgentCommand, GoalPhase, IdempotencyKey, KernelCommandPayload, OperationalStatus,
    PrincipalId, RuntimeEvent, SessionId, StoreError, TaskLifecycle,
};

const HARNESS_DISPATCHER_CLAIMANT: &str = "harness-dispatcher-v1";
const MAX_PRE_ACCEPT_ATTEMPTS: u64 = 3;

pub struct AgentCommandDispatcher {
    kernel: Arc<AgentKernel>,
    registry: Arc<HarnessRegistry>,
    approvals: Arc<dyn HarnessApprovalBroker>,
    routes: HarnessRouteTable,
    active: Arc<Mutex<BTreeSet<HarnessInvocationId>>>,
}

impl AgentKernel {
    #[must_use]
    pub fn with_harness_registry(
        self: Arc<Self>,
        registry: Arc<HarnessRegistry>,
        approvals: Arc<dyn HarnessApprovalBroker>,
    ) -> AgentCommandDispatcher {
        let builtin =
            HarnessProfileId::new("builtin-v1").expect("the built-in harness profile id is valid");
        AgentCommandDispatcher {
            kernel: self,
            registry,
            approvals,
            routes: HarnessRouteTable::new(builtin.clone(), builtin),
            active: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }
}

impl AgentCommandDispatcher {
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
        let profile_id = self
            .routes
            .select(execution.task.preferences.harness_role, payload.role)
            .clone();
        let profile_digest = self
            .registry
            .profile_digest(&profile_id)
            .map_err(|error| StoreError::InvalidData(error.to_string()))?;
        let candidate = HarnessInvocationId::new(uuid::Uuid::now_v7());
        let prepared = self
            .kernel
            .store()
            .prepare_harness_invocation(PrepareHarnessInvocation {
                invocation_id: &candidate,
                command_id: &claimed.id,
                command_revision: claimed.revision,
                profile_id: &profile_id,
                profile_digest: &profile_digest,
                payload_digest: &claimed.payload_digest,
            })
            .await?;
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
                let prepared = self
                    .kernel
                    .store()
                    .mark_harness_prompt_accepted(&prepared.invocation_id, prepared.attempt)
                    .await?;
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
                let terminal = self
                    .registry
                    .invoke(
                        request,
                        self.approvals.clone(),
                        Arc::new(PostgresHarnessEventSink::new(self.kernel.store_arc())),
                    )
                    .await;
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
        let terminal_key = terminal_idempotency_key(&invocation.invocation_id);
        let terminal_payload = terminal_payload(&terminal);
        match terminal {
            HarnessTerminal::Completed(output) => {
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
                complete_or_audit_late(&self.kernel, &execution.task.principal, &completion).await
            }
            HarnessTerminal::Cancelled => {
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
                let failure =
                    failure_request(execution, claimed, &terminal_key, "harness_cancelled")?;
                failure_or_audit_late(&self.kernel, &execution.task.principal, &failure).await
            }
            HarnessTerminal::Retryable(_) => {
                let retry = self
                    .kernel
                    .store()
                    .record_harness_retryable_failure(
                        &invocation.invocation_id,
                        invocation.attempt,
                        MAX_PRE_ACCEPT_ATTEMPTS,
                        &terminal_key,
                        &terminal_payload,
                    )
                    .await?;
                if retry.terminal {
                    let failure =
                        failure_request(execution, claimed, &terminal_key, "harness_failed")?;
                    failure_or_audit_late(&self.kernel, &execution.task.principal, &failure)
                        .await?;
                }
                Ok(())
            }
            HarnessTerminal::Failed(failure) => {
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
                let failure = failure_request(execution, claimed, &terminal_key, failure.code)?;
                failure_or_audit_late(&self.kernel, &execution.task.principal, &failure).await
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

    async fn settle_outcome_unknown(
        &self,
        execution: &crate::AgentExecutionView,
        claimed: &crate::AgentCommandView,
        invocation_id: &HarnessInvocationId,
        attempt: u64,
    ) -> Result<(), StoreError> {
        let terminal_key = terminal_idempotency_key(invocation_id);
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
        let failure =
            failure_request(execution, claimed, &terminal_key, "harness_outcome_unknown")?;
        failure_or_audit_late(&self.kernel, &execution.task.principal, &failure).await
    }
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

    use super::{ActiveDispatchGuard, DispatchRoute, dispatch_route, terminal_from_record};
    use crate::harness::{HarnessInvocationId, HarnessProfileId, HarnessTerminal};
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
