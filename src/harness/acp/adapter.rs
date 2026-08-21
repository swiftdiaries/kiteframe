use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::builtin::{DURABLE_GOAL_DIRECTIVE, validate_schema_completion};
use super::super::{
    AcpCommand, AcpLimits, HARNESS_PROTOCOL_VERSION, HarnessApprovalBroker, HarnessApprovalRequest,
    HarnessCancellation, HarnessCapabilities, HarnessCapability, HarnessEventSender,
    HarnessFailure, HarnessInvocationEvent, HarnessInvocationOutput, HarnessPlugin, HarnessProfile,
    HarnessRequestEnvelope, HarnessTerminal, HarnessTransport, PermissionOption, emit_and_wait,
};
use super::client::{
    AcpError, AcpInbound, AcpStdioClient, ContentBlock, RequestPermission, SessionNotification,
    SessionUpdate, StopReason,
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

pub struct AcpHarnessPlugin {
    profile: HarnessProfile,
    command: AcpCommand,
    limits: AcpLimits,
    environment: BTreeMap<OsString, OsString>,
}

impl AcpHarnessPlugin {
    #[must_use]
    pub fn new(profile: HarnessProfile, command: AcpCommand, limits: AcpLimits) -> Self {
        Self {
            profile,
            command,
            limits,
            environment: BTreeMap::new(),
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
        }
    }

    #[must_use]
    pub fn pre_accept_attempt_limit(&self) -> u64 {
        u64::from(self.limits.max_pre_accept_attempts)
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
        let client = match AcpStdioClient::spawn(
            &self.command,
            &request.invocation.workspace,
            self.limits.clone(),
            &self.environment,
        )
        .await
        {
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

        let prompt_request = client.prompt(session.session_id.clone(), prompt);
        tokio::pin!(prompt_request);
        let mut assistant_text = String::new();
        let mut cancellation_sent = false;
        let terminal = loop {
            tokio::select! {
                _ = cancellation.cancelled(), if !cancellation_sent => {
                    cancellation_sent = true;
                    if client.cancel(session.session_id.clone()).await.is_err() {
                        client.kill().await;
                        break HarnessTerminal::Cancelled;
                    }
                }
                response = &mut prompt_request => {
                    match response {
                        Ok(response) if cancellation_sent => {
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
                                &mut assistant_text,
                            ).await {
                                break terminal;
                            }
                            break completion_terminal(&request, &assistant_text);
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
                                &mut assistant_text,
                            ).await {
                                Ok(()) => {}
                                Err(error) => break HarnessTerminal::OutcomeUnknown(error),
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
                    break if cancellation_sent {
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
        let _ = client.shutdown().await;
        terminal
    }
}

async fn drain_pre_response_updates(
    client: &AcpStdioClient,
    session_id: &str,
    events: &HarnessEventSender,
    assistant_text: &mut String,
) -> Result<(), HarnessTerminal> {
    loop {
        match client.try_recv().await {
            Ok(Some(AcpInbound::Notification { method, params })) if method == "session/update" => {
                handle_session_update(session_id, params, events, assistant_text)
                    .await
                    .map_err(HarnessTerminal::OutcomeUnknown)?;
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
        let client = AcpStdioClient::spawn(
            &self.command,
            &workspace,
            self.limits.clone(),
            &self.environment,
        )
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
        let capabilities = observed_capabilities(&initialize)?;
        tokio::task::yield_now().await;
        if let Some(error) = client.connection_failure().await
            && matches!(error, AcpError::Protocol(_))
        {
            let _ = client.shutdown().await;
            return Err(describe_error(error));
        }
        let _ = client.shutdown().await;
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
    assistant_text: &mut String,
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
        assistant_text.push_str(&text);
        emit_and_wait(events, HarnessInvocationEvent::AssistantMessage { text }).await?;
        Ok(())
    } else {
        Err(HarnessFailure::new(
            "harness_capability_unsupported",
            "ACP emitted non-text or rich session output",
        ))
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
) -> Result<Option<HarnessTerminal>, HarnessTerminal> {
    let request: RequestPermission = serde_json::from_value(params).map_err(|error| {
        HarnessTerminal::Failed(HarnessFailure::new(
            "harness_protocol_incompatible",
            error.to_string(),
        ))
    })?;
    if request.session_id != expected_session {
        return Err(HarnessTerminal::Failed(HarnessFailure::new(
            "harness_protocol_incompatible",
            "ACP permission session id did not match session/new",
        )));
    }
    let request_id = match &id {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => {
            return Err(HarnessTerminal::Failed(HarnessFailure::new(
                "harness_protocol_incompatible",
                "ACP permission request id is invalid",
            )));
        }
    };
    let title = request
        .tool_call
        .title
        .unwrap_or_else(|| "ACP agent permission request".to_owned());
    emit_and_wait(
        events,
        HarnessInvocationEvent::ApprovalRequested {
            request_id: request_id.clone(),
            title: title.clone(),
        },
    )
    .await
    .map_err(HarnessTerminal::OutcomeUnknown)?;
    let approval = HarnessApprovalRequest {
        invocation_id: invocation.invocation.invocation_id.clone(),
        request_id: request_id.clone(),
        title,
    };
    let decision = tokio::select! {
        _ = cancellation.cancelled() => {
            let _ = client.cancel(expected_session.to_owned()).await;
            let _ = client.respond(id, json!({"outcome":{"outcome":"cancelled"}})).await;
            return Ok(Some(HarnessTerminal::Cancelled));
        },
        decision = approvals.decide(approval) => decision,
    };
    emit_and_wait(
        events,
        HarnessInvocationEvent::ApprovalResolved {
            request_id,
            decision: decision.clone(),
        },
    )
    .await
    .map_err(HarnessTerminal::OutcomeUnknown)?;
    let options = request
        .options
        .into_iter()
        .map(|option| PermissionOption {
            id: option.option_id,
            kind: option.kind,
        })
        .collect::<Vec<_>>();
    let option_id =
        select_permission_once(&options, decision.clone()).map_err(HarnessTerminal::Failed)?;
    client
        .respond(
            id,
            json!({"outcome":{"outcome":"selected","optionId":option_id}}),
        )
        .await
        .map_err(post_accept_error)?;
    if decision == super::super::HarnessApprovalDecision::RejectOnce {
        return Ok(Some(HarnessTerminal::Failed(HarnessFailure::new(
            "harness_permission_rejected",
            "ACP permission request was rejected once",
        ))));
    }
    Ok(None)
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
