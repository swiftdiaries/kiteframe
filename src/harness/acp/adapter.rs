use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::builtin::{DURABLE_GOAL_DIRECTIVE, validate_schema_completion};
use super::super::{
    AcpCommand, AcpLimits, HARNESS_PROTOCOL_VERSION, HarnessApprovalBroker,
    HarnessApprovalDecision, HarnessApprovalRequest, HarnessCancellation, HarnessCapabilities,
    HarnessCapability, HarnessEventSender, HarnessFailure, HarnessInvocationEvent,
    HarnessInvocationOutput, HarnessPlugin, HarnessProfile, HarnessRequestEnvelope,
    HarnessTerminal, HarnessTransport, PermissionOption, emit_and_wait,
};
use super::client::{
    AcpError, AcpInbound, AcpStdioClient, ContentBlock, RequestPermission, SessionNotification,
    SessionUpdate, StopReason, canonical_request_id,
};
use super::permissions::select_permission_once;
use crate::{RuntimeHistoryItem, RuntimeHistoryKind, canonical_json};

const PORTABLE_CORE: [HarnessCapability; 5] = [
    HarnessCapability::FreshSession,
    HarnessCapability::TextPrompt,
    HarnessCapability::PermissionOnce,
    HarnessCapability::Cancel,
    HarnessCapability::CommittedOutput,
];
const PERMISSION_CLEANUP_RESERVE: Duration = Duration::from_millis(25);
const MAX_ASSISTANT_UPDATES: usize = 64;

#[derive(Default)]
struct CancelOnce {
    sent: AtomicBool,
}

struct AssistantOutput {
    text: String,
    updates: usize,
    max_bytes: u64,
    max_updates: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromptState {
    Ready,
    InFlight,
    Cancelling,
}

impl AssistantOutput {
    fn new(max_bytes: u64) -> Self {
        Self {
            text: String::new(),
            updates: 0,
            max_bytes,
            max_updates: usize::try_from(max_bytes)
                .unwrap_or(usize::MAX)
                .min(MAX_ASSISTANT_UPDATES),
        }
    }

    fn append(&mut self, text: &str) -> Result<(), HarnessFailure> {
        if self.updates >= self.max_updates {
            return Err(HarnessFailure::new(
                "harness_failed",
                "ACP assistant update count exceeded the closed invocation limit",
            ));
        }
        let next_bytes = self.text.len().checked_add(text.len()).ok_or_else(|| {
            HarnessFailure::new("harness_failed", "ACP assistant byte count overflowed")
        })?;
        if u64::try_from(next_bytes).unwrap_or(u64::MAX) > self.max_bytes {
            return Err(HarnessFailure::new(
                "harness_failed",
                "ACP assistant text exceeded response_item_bytes",
            ));
        }
        self.updates += 1;
        self.text.push_str(text);
        Ok(())
    }
}

impl CancelOnce {
    async fn send(
        &self,
        client: &AcpStdioClient,
        session_id: &str,
        deadline: tokio::time::Instant,
    ) -> Result<(), AcpError> {
        if self
            .sent
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        tokio::time::timeout_at(deadline, client.cancel(session_id.to_owned()))
            .await
            .map_err(|_| AcpError::Limit("ACP cancel write exceeded active deadline".to_owned()))?
    }
}

pub struct AcpHarnessPlugin {
    profile: HarnessProfile,
    command: AcpCommand,
    limits: AcpLimits,
    environment: BTreeMap<OsString, OsString>,
    #[cfg(feature = "test-support")]
    test_barrier: Option<Arc<super::client::AcpTestBarrier>>,
}

impl AcpHarnessPlugin {
    #[must_use]
    pub fn new(profile: HarnessProfile, command: AcpCommand, limits: AcpLimits) -> Self {
        Self {
            profile,
            command,
            limits,
            environment: BTreeMap::new(),
            #[cfg(feature = "test-support")]
            test_barrier: None,
        }
    }

    /// Construct an adapter with a closed child environment.
    ///
    /// Only the explicitly supplied entries are inherited by the child process; ambient
    /// process variables are cleared before spawn.
    #[must_use]
    pub fn with_environment(
        profile: HarnessProfile,
        command: AcpCommand,
        limits: AcpLimits,
        environment: BTreeMap<OsString, OsString>,
    ) -> Self {
        Self {
            profile,
            command,
            limits,
            environment,
            #[cfg(feature = "test-support")]
            test_barrier: None,
        }
    }

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn with_test_barrier(mut self, barrier: Arc<super::client::AcpTestBarrier>) -> Self {
        self.test_barrier = Some(barrier);
        self
    }

    #[must_use]
    pub fn pre_accept_attempt_limit(&self) -> u64 {
        u64::from(self.limits.max_pre_accept_attempts)
    }

    async fn spawn_client(&self, workspace: &std::path::Path) -> Result<AcpStdioClient, AcpError> {
        #[cfg(feature = "test-support")]
        if let Some(barrier) = &self.test_barrier {
            return AcpStdioClient::spawn_with_test_barrier(
                &self.command,
                workspace,
                self.limits.clone(),
                &self.environment,
                barrier.clone(),
            )
            .await;
        }
        AcpStdioClient::spawn(
            &self.command,
            workspace,
            self.limits.clone(),
            &self.environment,
        )
        .await
    }

    fn validate_configuration(&self) -> Result<(), HarnessFailure> {
        if self.profile.transport != HarnessTransport::AcpStdio {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "ACP adapter requires the acp_stdio transport",
            ));
        }
        if self.command.source_revision != self.profile.source_revision {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "ACP command source revision does not match profile attestation",
            ));
        }
        if self.limits.max_frame_bytes == 0 || self.limits.max_pre_accept_attempts == 0 {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "ACP limits must permit at least one bounded attempt and frame",
            ));
        }
        let core = BTreeSet::from(PORTABLE_CORE);
        if !self.profile.required.is_subset(&core) {
            return Err(HarnessFailure::new(
                "harness_capability_unsupported",
                "ACP profile requires an unnegotiated rich capability",
            ));
        }
        Ok(())
    }

    async fn run(
        &self,
        request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        approvals: Arc<dyn HarnessApprovalBroker>,
        cancellation: HarnessCancellation,
    ) -> HarnessTerminal {
        if let Err(error) = emit_and_wait(&events, HarnessInvocationEvent::Started).await {
            return HarnessTerminal::OutcomeUnknown(error);
        }
        if cancellation.is_cancelled() {
            return HarnessTerminal::Cancelled;
        }
        if !request.invocation.workspace.is_absolute() {
            return HarnessTerminal::Failed(HarnessFailure::new(
                "harness_profile_unavailable",
                "ACP invocation workspace must be absolute",
            ));
        }
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(
                request
                    .invocation
                    .payload
                    .limits
                    .active_execution_millis_remaining,
            );
        let client = match self.spawn_client(&request.invocation.workspace).await {
            Ok(client) => client,
            Err(error) => return pre_accept_error(error),
        };

        let initialize =
            match wait_pre_accept(&client, cancellation.clone(), deadline, client.initialize())
                .await
            {
                Ok(response) => response,
                Err(terminal) => {
                    let _ = client.shutdown().await;
                    return terminal;
                }
            };
        let observed = match observed_capabilities(&initialize) {
            Ok(observed) => observed,
            Err(error) => {
                let _ = client.shutdown().await;
                return HarnessTerminal::Failed(error);
            }
        };
        if let Err(error) = self.profile.validate_observed(&observed) {
            let _ = client.shutdown().await;
            return HarnessTerminal::Failed(error);
        }

        let session = match wait_pre_accept(
            &client,
            cancellation.clone(),
            deadline,
            client.new_session(request.invocation.workspace.clone()),
        )
        .await
        {
            Ok(response) => response,
            Err(terminal) => {
                let _ = client.shutdown().await;
                return terminal;
            }
        };
        if session.session_id.is_empty() {
            let _ = client.shutdown().await;
            return HarnessTerminal::Failed(HarnessFailure::new(
                "harness_protocol_incompatible",
                "ACP session/new returned an empty session id",
            ));
        }
        let prompt = match canonical_prompt(&request.invocation.payload) {
            Ok(prompt) => prompt,
            Err(error) => {
                let _ = client.shutdown().await;
                return HarnessTerminal::Failed(error);
            }
        };
        if let Err(error) = events.commit_acceptance().await {
            let _ = client.shutdown().await;
            return HarnessTerminal::OutcomeUnknown(error);
        }

        let cancel_once = Arc::new(CancelOnce::default());
        let mut prompt_state = PromptState::Ready;
        let mut prompt_request = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                let _ = client.shutdown().await;
                return HarnessTerminal::Cancelled;
            }
            _ = tokio::time::sleep_until(deadline) => {
                client.kill().await;
                return HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
                    "harness_outcome_unknown",
                    "ACP active execution limit expired after prompt acceptance",
                ));
            }
            request = client.prepare_prompt(session.session_id.clone(), prompt) => match request {
                Ok(request) => request,
                Err(error) => {
                    let _ = client.shutdown().await;
                    return post_accept_error(error);
                }
            }
        };
        debug_assert_eq!(prompt_state, PromptState::Ready);
        let write = tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            _ = tokio::time::sleep_until(deadline) => {
                client.kill().await;
                return HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
                    "harness_outcome_unknown",
                    "ACP active execution limit expired while sending the accepted prompt",
                ));
            }
            result = prompt_request.write() => Some(result),
        };
        if write.is_none() {
            prompt_state = PromptState::Cancelling;
            if !prompt_request.cancel_before_write()
                && cancel_once
                    .send(&client, &session.session_id, deadline)
                    .await
                    .is_err()
            {
                client.kill().await;
            }
            drop(prompt_request);
            debug_assert_eq!(prompt_state, PromptState::Cancelling);
            let _ = client.shutdown().await;
            return HarnessTerminal::Cancelled;
        }
        if let Err(error) = write.expect("prompt write branch is present") {
            drop(prompt_request);
            let _ = client.shutdown().await;
            return post_accept_error(error);
        }
        prompt_state = PromptState::InFlight;
        let prompt_request = prompt_request.response::<super::client::PromptResponse>();
        tokio::pin!(prompt_request);
        let mut assistant_output =
            AssistantOutput::new(request.invocation.payload.limits.response_item_bytes);
        let terminal = loop {
            if cancellation.is_cancelled() && prompt_state == PromptState::InFlight {
                prompt_state = PromptState::Cancelling;
                if cancel_once
                    .send(&client, &session.session_id, deadline)
                    .await
                    .is_err()
                {
                    client.kill().await;
                    break HarnessTerminal::Cancelled;
                }
                continue;
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled(), if prompt_state == PromptState::InFlight => {
                    prompt_state = PromptState::Cancelling;
                    if cancel_once.send(&client, &session.session_id, deadline).await.is_err() {
                        client.kill().await;
                        break HarnessTerminal::Cancelled;
                    }
                }
                response = &mut prompt_request => {
                    match response {
                        Ok(response) if prompt_state == PromptState::Cancelling => {
                            let _ = response;
                            break HarnessTerminal::Cancelled;
                        }
                        Ok(response) if response.stop_reason == StopReason::Cancelled => {
                            break HarnessTerminal::Cancelled;
                        }
                        Ok(response) if response.stop_reason == StopReason::EndTurn => {
                            if let Err(terminal) = drain_pre_response_updates(
                                &client,
                                &session.session_id,
                                &events,
                                &mut assistant_output,
                            ).await {
                                break terminal;
                            }
                            if cancellation.is_cancelled() {
                                if cancel_once
                                    .send(&client, &session.session_id, deadline)
                                    .await
                                    .is_err()
                                {
                                    client.kill().await;
                                }
                                break HarnessTerminal::Cancelled;
                            }
                            break completion_terminal(&request, &assistant_output.text);
                        }
                        Ok(response) => {
                            break HarnessTerminal::Failed(HarnessFailure::new(
                                "harness_failed",
                                format!("ACP prompt stopped with {:?}", response.stop_reason),
                            ));
                        }
                        Err(error) => break post_accept_error(error),
                    }
                }
                incoming = client.recv() => {
                    match incoming {
                        Ok(AcpInbound::Notification { method, params }) if method == "session/update" => {
                            match handle_session_update(
                                &session.session_id,
                                params,
                                &events,
                                &mut assistant_output,
                            ).await {
                                Ok(()) => {}
                                Err(error) => {
                                    client.kill().await;
                                    break update_failure_terminal(error);
                                },
                            }
                        }
                        Ok(AcpInbound::Request { id, method, params }) if method == "session/request_permission" => {
                            match handle_permission(
                                &client,
                                &request,
                                &session.session_id,
                                id,
                                params,
                                &events,
                                approvals.clone(),
                                cancellation.clone(),
                                cancel_once.clone(),
                                deadline,
                            ).await {
                                Ok(Some(terminal)) => break terminal,
                                Ok(None) => {}
                                Err(terminal) => break terminal,
                            }
                        }
                        Ok(_) => break HarnessTerminal::Failed(HarnessFailure::new(
                            "harness_capability_unsupported",
                            "ACP agent used an unnegotiated method or notification",
                        )),
                        Err(error) => break post_accept_error(error),
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    client.kill().await;
                    break if prompt_state == PromptState::Cancelling {
                        HarnessTerminal::Cancelled
                    } else {
                        HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
                            "harness_outcome_unknown",
                            "ACP active execution limit expired after prompt acceptance",
                        ))
                    };
                }
            }
        };
        if matches!(terminal, HarnessTerminal::Completed(_))
            && let Err(error) = client.verify_alive_before_shutdown().await
        {
            let _ = client.shutdown().await;
            return post_accept_error(error);
        }
        let shutdown = client.shutdown().await;
        match (terminal, shutdown) {
            (HarnessTerminal::Completed(_), Err(error)) => post_accept_error(error),
            (terminal, Ok(())) => terminal,
            (terminal, Err(_)) => terminal,
        }
    }
}

async fn drain_pre_response_updates(
    client: &AcpStdioClient,
    session_id: &str,
    events: &HarnessEventSender,
    assistant_output: &mut AssistantOutput,
) -> Result<(), HarnessTerminal> {
    loop {
        match client.try_recv().await {
            Ok(Some(AcpInbound::Notification { method, params })) if method == "session/update" => {
                if let Err(error) =
                    handle_session_update(session_id, params, events, assistant_output).await
                {
                    client.kill().await;
                    return Err(update_failure_terminal(error));
                }
            }
            Ok(Some(_)) => {
                return Err(HarnessTerminal::Failed(HarnessFailure::new(
                    "harness_capability_unsupported",
                    "ACP agent used an unnegotiated method before prompt completion",
                )));
            }
            Err(AcpError::ConnectionLost(_)) | Ok(None) => return Ok(()),
            Err(error) => return Err(post_accept_error(error)),
        }
    }
}

#[async_trait::async_trait]
impl HarnessPlugin for AcpHarnessPlugin {
    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    async fn describe_capabilities(&self) -> Result<HarnessCapabilities, HarnessFailure> {
        self.validate_configuration()?;
        let workspace = std::env::current_dir().map_err(|error| {
            HarnessFailure::new("harness_profile_unavailable", error.to_string())
        })?;
        let client = self
            .spawn_client(&workspace)
            .await
            .map_err(describe_error)?;
        let initialize = match tokio::time::timeout(
            self.limits.shutdown_grace.max(Duration::from_secs(1)),
            client.initialize(),
        )
        .await
        {
            Ok(Ok(initialize)) => initialize,
            Ok(Err(error)) => {
                let _ = client.shutdown().await;
                return Err(describe_error(error));
            }
            Err(_) => {
                client.kill().await;
                return Err(HarnessFailure::new(
                    "harness_profile_unavailable",
                    "ACP initialize timed out during registration",
                ));
            }
        };
        let capabilities = match observed_capabilities(&initialize) {
            Ok(capabilities) => capabilities,
            Err(error) => {
                let _ = client.shutdown().await;
                return Err(error);
            }
        };
        if let Err(error) = client.verify_alive_before_shutdown().await {
            let _ = client.shutdown().await;
            return Err(describe_error(error));
        }
        client.shutdown().await.map_err(describe_error)?;
        Ok(capabilities)
    }

    async fn start_invocation(
        &self,
        request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        approvals: Arc<dyn HarnessApprovalBroker>,
        cancellation: HarnessCancellation,
    ) -> HarnessTerminal {
        if let Err(error) = self.validate_configuration() {
            return HarnessTerminal::Failed(error);
        }
        self.run(request, events, approvals, cancellation).await
    }
}

async fn wait_pre_accept<T>(
    client: &AcpStdioClient,
    cancellation: HarnessCancellation,
    deadline: tokio::time::Instant,
    future: impl std::future::Future<Output = Result<T, AcpError>>,
) -> Result<T, HarnessTerminal> {
    tokio::select! {
        _ = cancellation.cancelled() => {
            client.kill().await;
            Err(HarnessTerminal::Cancelled)
        }
        _ = tokio::time::sleep_until(deadline) => {
            client.kill().await;
            Err(HarnessTerminal::Retryable(HarnessFailure::new(
                "harness_failed",
                "ACP active execution limit expired before prompt acceptance",
            )))
        }
        result = future => result.map_err(pre_accept_error),
    }
}

async fn handle_session_update(
    expected_session: &str,
    params: Value,
    events: &HarnessEventSender,
    assistant_output: &mut AssistantOutput,
) -> Result<(), HarnessFailure> {
    let notification: SessionNotification = serde_json::from_value(params)
        .map_err(|error| HarnessFailure::new("harness_protocol_incompatible", error.to_string()))?;
    if notification.session_id != expected_session {
        return Err(HarnessFailure::new(
            "harness_protocol_incompatible",
            "ACP update session id did not match session/new",
        ));
    }
    if let SessionUpdate::AgentMessageChunk {
        content: ContentBlock::Text { text, .. },
        ..
    } = notification.update
    {
        assistant_output.append(&text)?;
        emit_and_wait(events, HarnessInvocationEvent::AssistantMessage { text }).await?;
        Ok(())
    } else {
        Err(HarnessFailure::new(
            "harness_capability_unsupported",
            "ACP emitted non-text or rich session output",
        ))
    }
}

fn update_failure_terminal(error: HarnessFailure) -> HarnessTerminal {
    if error.code == "harness_outcome_unknown" {
        HarnessTerminal::OutcomeUnknown(error)
    } else {
        HarnessTerminal::Failed(error)
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_permission(
    client: &AcpStdioClient,
    invocation: &HarnessRequestEnvelope,
    expected_session: &str,
    id: Value,
    params: Value,
    events: &HarnessEventSender,
    approvals: Arc<dyn HarnessApprovalBroker>,
    cancellation: HarnessCancellation,
    cancel_once: Arc<CancelOnce>,
    deadline: tokio::time::Instant,
) -> Result<Option<HarnessTerminal>, HarnessTerminal> {
    let request_id = canonical_request_id(&id).map_err(|error| {
        HarnessTerminal::Failed(HarnessFailure::new(
            "harness_protocol_incompatible",
            error.to_string(),
        ))
    })?;
    let request: RequestPermission = match serde_json::from_value(params) {
        Ok(request) => request,
        Err(error) => {
            return reject_invalid_permission(
                client,
                id,
                request_id,
                "ACP agent permission request",
                events,
                deadline,
                HarnessFailure::new("harness_protocol_incompatible", error.to_string()),
            )
            .await;
        }
    };
    let title = request
        .tool_call
        .title
        .unwrap_or_else(|| "ACP agent permission request".to_owned());
    if request.session_id != expected_session {
        return reject_invalid_permission(
            client,
            id,
            request_id,
            &title,
            events,
            deadline,
            HarnessFailure::new(
                "harness_protocol_incompatible",
                "ACP permission session id did not match session/new",
            ),
        )
        .await;
    }
    emit_before_deadline(
        events,
        HarnessInvocationEvent::ApprovalRequested {
            request_id: request_id.clone(),
            title: title.clone(),
        },
        deadline,
    )
    .await
    .map_err(HarnessTerminal::OutcomeUnknown)?;
    let approval = HarnessApprovalRequest {
        invocation_id: invocation.invocation.invocation_id.clone(),
        request_id: request_id.clone(),
        title,
    };
    let mut cancelled = cancellation.is_cancelled();
    let mut approval_timed_out = false;
    let decision_deadline = deadline
        .checked_sub(PERMISSION_CLEANUP_RESERVE)
        .unwrap_or(deadline);
    let decision = if cancelled {
        HarnessApprovalDecision::RejectOnce
    } else {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                cancelled = true;
                HarnessApprovalDecision::RejectOnce
            },
            _ = tokio::time::sleep_until(decision_deadline) => {
                approval_timed_out = true;
                HarnessApprovalDecision::RejectOnce
            },
            decision = approvals.decide(approval) => decision,
        }
    };
    emit_before_deadline(
        events,
        HarnessInvocationEvent::ApprovalResolved {
            request_id,
            decision: decision.clone(),
        },
        deadline,
    )
    .await
    .map_err(HarnessTerminal::OutcomeUnknown)?;
    if cancelled {
        cancel_once
            .send(client, expected_session, deadline)
            .await
            .map_err(post_accept_error)?;
        tokio::time::timeout_at(
            deadline,
            client.respond(id, json!({"outcome":{"outcome":"cancelled"}})),
        )
        .await
        .map_err(|_| {
            post_accept_error(AcpError::Limit(
                "ACP permission cancellation response exceeded active deadline".to_owned(),
            ))
        })?
        .map_err(post_accept_error)?;
        return Ok(Some(HarnessTerminal::Cancelled));
    }
    if approval_timed_out {
        respond_error_before_deadline(client, id, -32002, "approval deadline exceeded", deadline)
            .await
            .map_err(post_accept_error)?;
        return Ok(Some(HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
            "harness_outcome_unknown",
            "ACP approval broker exceeded the active execution deadline",
        ))));
    }
    let options = request
        .options
        .into_iter()
        .map(|option| PermissionOption {
            id: option.option_id,
            kind: option.kind,
        })
        .collect::<Vec<_>>();
    let option_id = match select_permission_once(&options, decision.clone()) {
        Ok(option_id) => option_id,
        Err(error) => {
            respond_error_before_deadline(
                client,
                id,
                -32001,
                "required one-shot permission option unavailable",
                deadline,
            )
            .await
            .map_err(post_accept_error)?;
            return Err(HarnessTerminal::Failed(error));
        }
    };
    tokio::time::timeout_at(
        deadline,
        client.respond(
            id,
            json!({"outcome":{"outcome":"selected","optionId":option_id}}),
        ),
    )
    .await
    .map_err(|_| {
        post_accept_error(AcpError::Limit(
            "ACP permission response exceeded active deadline".to_owned(),
        ))
    })?
    .map_err(post_accept_error)?;
    if decision == super::super::HarnessApprovalDecision::RejectOnce {
        return Ok(Some(HarnessTerminal::Failed(HarnessFailure::new(
            "harness_permission_rejected",
            "ACP permission request was rejected once",
        ))));
    }
    Ok(None)
}

async fn reject_invalid_permission(
    client: &AcpStdioClient,
    id: Value,
    request_id: String,
    title: &str,
    events: &HarnessEventSender,
    deadline: tokio::time::Instant,
    failure: HarnessFailure,
) -> Result<Option<HarnessTerminal>, HarnessTerminal> {
    emit_before_deadline(
        events,
        HarnessInvocationEvent::ApprovalRequested {
            request_id: request_id.clone(),
            title: title.to_owned(),
        },
        deadline,
    )
    .await
    .map_err(HarnessTerminal::OutcomeUnknown)?;
    emit_before_deadline(
        events,
        HarnessInvocationEvent::ApprovalResolved {
            request_id,
            decision: HarnessApprovalDecision::RejectOnce,
        },
        deadline,
    )
    .await
    .map_err(HarnessTerminal::OutcomeUnknown)?;
    respond_error_before_deadline(client, id, -32602, "invalid permission request", deadline)
        .await
        .map_err(post_accept_error)?;
    Err(HarnessTerminal::Failed(failure))
}

async fn emit_before_deadline(
    events: &HarnessEventSender,
    event: HarnessInvocationEvent,
    deadline: tokio::time::Instant,
) -> Result<(), HarnessFailure> {
    tokio::time::timeout_at(deadline, emit_and_wait(events, event))
        .await
        .map_err(|_| {
            HarnessFailure::new(
                "harness_outcome_unknown",
                "ACP durable permission event exceeded active deadline",
            )
        })?
}

async fn respond_error_before_deadline(
    client: &AcpStdioClient,
    id: Value,
    code: i64,
    message: &'static str,
    deadline: tokio::time::Instant,
) -> Result<(), AcpError> {
    tokio::time::timeout_at(deadline, client.respond_error(id, code, message))
        .await
        .map_err(|_| AcpError::Limit("ACP error response exceeded active deadline".to_owned()))?
}

fn canonical_prompt(invocation: &crate::InvokeHarness) -> Result<String, HarnessFailure> {
    let contract = canonical_json(invocation)
        .map_err(|error| HarnessFailure::new("harness_failed", error.to_string()))?;
    let contract = String::from_utf8(contract)
        .map_err(|error| HarnessFailure::new("harness_failed", error.to_string()))?;
    Ok(format!("{DURABLE_GOAL_DIRECTIVE}\n\n{contract}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictCompletion {
    outcome: String,
    fields: Value,
}

fn completion_terminal(request: &HarnessRequestEnvelope, text: &str) -> HarnessTerminal {
    if text.len() as u64 > request.invocation.payload.limits.response_item_bytes {
        return HarnessTerminal::Failed(HarnessFailure::new(
            "harness_failed",
            "ACP committed assistant result exceeded response_item_bytes",
        ));
    }
    let completion: StrictCompletion = match serde_json::from_str(text) {
        Ok(completion) => completion,
        Err(error) => {
            return HarnessTerminal::Failed(HarnessFailure::new(
                "harness_failed",
                format!("ACP committed assistant result is not one strict JSON object: {error}"),
            ));
        }
    };
    if !completion.fields.is_object() {
        return HarnessTerminal::Failed(HarnessFailure::new(
            "harness_failed",
            "ACP completion fields must be an object",
        ));
    }
    if let Err(error) = validate_schema_completion(
        &request.invocation.payload.result_schema,
        &completion.outcome,
        &completion.fields,
    ) {
        return HarnessTerminal::Failed(HarnessFailure::new("harness_failed", error.to_string()));
    }
    let payload = json!({"outcome":completion.outcome,"fields":completion.fields});
    HarnessTerminal::Completed(HarnessInvocationOutput {
        outcome: payload["outcome"].as_str().unwrap().to_owned(),
        fields: payload["fields"].clone(),
        history: vec![RuntimeHistoryItem {
            sequence: 0,
            kind: RuntimeHistoryKind::HarnessOutput,
            payload,
        }],
    })
}

fn pre_accept_error(error: AcpError) -> HarnessTerminal {
    match error {
        AcpError::Protocol(detail) => {
            HarnessTerminal::Failed(HarnessFailure::new("harness_protocol_incompatible", detail))
        }
        error => {
            HarnessTerminal::Retryable(HarnessFailure::new("harness_failed", error.to_string()))
        }
    }
}

fn describe_error(error: AcpError) -> HarnessFailure {
    match error {
        AcpError::Protocol(detail) => HarnessFailure::new("harness_protocol_incompatible", detail),
        error => HarnessFailure::new("harness_profile_unavailable", error.to_string()),
    }
}

fn observed_capabilities(
    initialize: &super::client::InitializeResponse,
) -> Result<HarnessCapabilities, HarnessFailure> {
    if initialize.protocol_version != HARNESS_PROTOCOL_VERSION {
        return Err(HarnessFailure::new(
            "harness_protocol_incompatible",
            format!(
                "expected ACP protocol {}, observed {}",
                HARNESS_PROTOCOL_VERSION, initialize.protocol_version
            ),
        ));
    }
    if initialize
        .agent_info
        .as_ref()
        .is_some_and(|identity| identity.name.is_empty() || identity.version.is_empty())
    {
        return Err(HarnessFailure::new(
            "harness_protocol_incompatible",
            "ACP agent identity fields must be non-empty when advertised",
        ));
    }
    let mut supported = BTreeSet::from(PORTABLE_CORE);
    for (capability, present) in initialize.agent_capabilities.optional_capabilities() {
        if present {
            supported.insert(capability);
        }
    }
    Ok(HarnessCapabilities {
        protocol_version: initialize.protocol_version,
        implementation_name: initialize.agent_info.as_ref().map_or_else(
            || "unreported-acp-agent".to_owned(),
            |info| info.name.clone(),
        ),
        implementation_version: initialize
            .agent_info
            .as_ref()
            .map_or_else(|| "unreported".to_owned(), |info| info.version.clone()),
        supported,
    })
}

fn post_accept_error(error: AcpError) -> HarnessTerminal {
    HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
        "harness_outcome_unknown",
        error.to_string(),
    ))
}
