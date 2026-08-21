use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kiteframe::harness::{
    DenyAllHarnessApprovals, HarnessApprovalBroker, HarnessApprovalDecision,
    HarnessApprovalRequest, HarnessCapabilities, HarnessCapability, HarnessEventEnvelope,
    HarnessEventSender, HarnessEventSink, HarnessFailure, HarnessInvocationEvent,
    HarnessInvocationId, HarnessInvocationOutput, HarnessInvocationRequest, HarnessPlugin,
    HarnessProfile, HarnessProfileId, HarnessRegistry, HarnessRequestEnvelope, HarnessTerminal,
    HarnessTransport, emit_and_wait,
};
use kiteframe::store::HarnessInvocationEventRecord;
use kiteframe::{
    AgentCommandId, ClosedSchema, GoalProjection, HarnessStructuralLimits, InvokeHarness, ModelRole,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn profile() -> HarnessProfile {
    HarnessProfile {
        id: HarnessProfileId::new("test-harness-v1").unwrap(),
        transport: HarnessTransport::Builtin,
        source_repository: "https://example.test/harness".to_owned(),
        source_revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        program: "test-harness".to_owned(),
        args: vec![],
        required: BTreeSet::from([
            HarnessCapability::FreshSession,
            HarnessCapability::TextPrompt,
            HarnessCapability::PermissionOnce,
            HarnessCapability::Cancel,
            HarnessCapability::CommittedOutput,
        ]),
        optional: BTreeSet::new(),
        fresh_sessions_only: true,
    }
}

fn request() -> HarnessInvocationRequest {
    HarnessInvocationRequest {
        invocation_id: HarnessInvocationId::new(Uuid::now_v7()),
        command_id: AgentCommandId::new(Uuid::now_v7()),
        command_revision: 0,
        profile_id: profile().id,
        workspace: "/workspace".into(),
        payload: InvokeHarness {
            role: ModelRole::Reasoning,
            prompt: "test".to_owned(),
            goal: GoalProjection {
                objective: "test".to_owned(),
                success: vec![],
                constraints: vec![],
                blocked_when: vec![],
            },
            input: json!({}),
            input_schema: ClosedSchema::object("input", Default::default()),
            result_schema: ClosedSchema::object("result", Default::default()),
            tools: vec![],
            projected_history: vec![],
            limits: HarnessStructuralLimits {
                model_turns_remaining: 1,
                tool_calls_per_response: 1,
                response_item_bytes: 1024,
                active_execution_millis_remaining: 1000,
            },
        },
        payload_digest: "sha256:payload".to_owned(),
    }
}

#[derive(Default)]
struct RecordingHarnessEventSink(Mutex<Vec<HarnessEventEnvelope>>);

impl RecordingHarnessEventSink {
    fn sequences(&self) -> Vec<u64> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.sequence)
            .collect()
    }
}

#[async_trait]
impl HarnessEventSink for RecordingHarnessEventSink {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

struct FailingHarnessEventSink;

#[async_trait]
impl HarnessEventSink for FailingHarnessEventSink {
    async fn append(&self, _event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        Err(HarnessFailure::new(
            "harness_event_persistence_failed",
            "test sink is unavailable",
        ))
    }
}

struct FailAfterOneHarnessEventSink(Mutex<usize>);

#[async_trait]
impl HarnessEventSink for FailAfterOneHarnessEventSink {
    async fn append(&self, _event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        let mut calls = self.0.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            Ok(())
        } else {
            Err(HarnessFailure::new(
                "harness_event_persistence_failed",
                "test sink failed after the first append",
            ))
        }
    }
}

enum FakeMode {
    Events(Vec<HarnessInvocationEvent>),
    WaitForCancel,
}

struct FakePlugin {
    profile: HarnessProfile,
    protocol_version: u16,
    mode: FakeMode,
}

struct BurstPlugin {
    profile: HarnessProfile,
    reports: mpsc::UnboundedSender<Result<(), HarnessFailure>>,
}

struct LateEmitterPlugin {
    profile: HarnessProfile,
    release: Arc<tokio::sync::Notify>,
    reports: mpsc::UnboundedSender<Result<(), HarnessFailure>>,
}

impl LateEmitterPlugin {
    fn new() -> (
        Arc<Self>,
        Arc<tokio::sync::Notify>,
        mpsc::UnboundedReceiver<Result<(), HarnessFailure>>,
    ) {
        let release = Arc::new(tokio::sync::Notify::new());
        let (reports, receiver) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                profile: profile(),
                release: release.clone(),
                reports,
            }),
            release,
            receiver,
        )
    }
}

impl BurstPlugin {
    fn new() -> (
        Arc<Self>,
        mpsc::UnboundedReceiver<Result<(), HarnessFailure>>,
    ) {
        let (reports, receiver) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                profile: profile(),
                reports,
            }),
            receiver,
        )
    }
}

impl FakePlugin {
    fn with_protocol_version(protocol_version: u16) -> Self {
        Self {
            profile: profile(),
            protocol_version,
            mode: FakeMode::Events(vec![]),
        }
    }

    fn events_then_success(events: Vec<HarnessInvocationEvent>) -> Self {
        Self {
            profile: profile(),
            protocol_version: 1,
            mode: FakeMode::Events(events),
        }
    }

    fn wait_for_cancel() -> Self {
        Self {
            profile: profile(),
            protocol_version: 1,
            mode: FakeMode::WaitForCancel,
        }
    }
}

#[async_trait]
impl HarnessPlugin for FakePlugin {
    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    async fn describe_capabilities(&self) -> Result<HarnessCapabilities, HarnessFailure> {
        Ok(HarnessCapabilities {
            protocol_version: self.protocol_version,
            implementation_name: "fake".to_owned(),
            implementation_version: "test".to_owned(),
            supported: self.profile.required.clone(),
        })
    }

    async fn start_invocation(
        &self,
        _request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        _approvals: Arc<dyn HarnessApprovalBroker>,
        cancellation: CancellationToken,
    ) -> HarnessTerminal {
        match &self.mode {
            FakeMode::Events(events_to_emit) => {
                for event in events_to_emit.clone() {
                    emit_and_wait(&events, event).await.unwrap();
                }
                HarnessTerminal::Completed(HarnessInvocationOutput {
                    outcome: "ok".to_owned(),
                    fields: json!({}),
                    history: vec![],
                })
            }
            FakeMode::WaitForCancel => {
                cancellation.cancelled().await;
                HarnessTerminal::Cancelled
            }
        }
    }
}

#[async_trait]
impl HarnessPlugin for BurstPlugin {
    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    async fn describe_capabilities(&self) -> Result<HarnessCapabilities, HarnessFailure> {
        Ok(HarnessCapabilities {
            protocol_version: 1,
            implementation_name: "burst".to_owned(),
            implementation_version: "test".to_owned(),
            supported: self.profile.required.clone(),
        })
    }

    async fn start_invocation(
        &self,
        _request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        _approvals: Arc<dyn HarnessApprovalBroker>,
        _cancellation: CancellationToken,
    ) -> HarnessTerminal {
        let mut acknowledgements = Vec::new();
        for event in [
            HarnessInvocationEvent::Started,
            HarnessInvocationEvent::Progress {
                message: "second".to_owned(),
            },
            HarnessInvocationEvent::Progress {
                message: "third".to_owned(),
            },
        ] {
            let (persisted, acknowledgement) = oneshot::channel();
            events
                .send(kiteframe::harness::HarnessPendingEvent { event, persisted })
                .await
                .unwrap();
            acknowledgements.push(acknowledgement);
        }
        let reports = self.reports.clone();
        tokio::spawn(async move {
            for acknowledgement in acknowledgements {
                let result = acknowledgement.await.unwrap_or_else(|_| {
                    Err(HarnessFailure::new(
                        "harness_event_persistence_failed",
                        "event acknowledgement was dropped",
                    ))
                });
                let _ = reports.send(result);
            }
        });
        HarnessTerminal::Completed(HarnessInvocationOutput {
            outcome: "ok".to_owned(),
            fields: json!({}),
            history: vec![],
        })
    }
}

#[async_trait]
impl HarnessPlugin for LateEmitterPlugin {
    fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    async fn describe_capabilities(&self) -> Result<HarnessCapabilities, HarnessFailure> {
        Ok(HarnessCapabilities {
            protocol_version: 1,
            implementation_name: "late-emitter".to_owned(),
            implementation_version: "test".to_owned(),
            supported: self.profile.required.clone(),
        })
    }

    async fn start_invocation(
        &self,
        _request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        _approvals: Arc<dyn HarnessApprovalBroker>,
        _cancellation: CancellationToken,
    ) -> HarnessTerminal {
        emit_and_wait(&events, HarnessInvocationEvent::Started)
            .await
            .unwrap();
        let release = self.release.clone();
        let reports = self.reports.clone();
        tokio::spawn(async move {
            release.notified().await;
            let _ = reports.send(
                emit_and_wait(
                    &events,
                    HarnessInvocationEvent::Progress {
                        message: "late".to_owned(),
                    },
                )
                .await,
            );
        });
        HarnessTerminal::Completed(HarnessInvocationOutput {
            outcome: "ok".to_owned(),
            fields: json!({}),
            history: vec![],
        })
    }
}

async fn registry_with(plugin: Arc<dyn HarnessPlugin>) -> HarnessRegistry {
    let mut registry = HarnessRegistry::new();
    registry.register(plugin).await.unwrap();
    registry
}

#[tokio::test]
async fn registry_rejects_capability_or_protocol_drift_before_invocation() {
    let plugin = Arc::new(FakePlugin::with_protocol_version(2));
    let mut registry = HarnessRegistry::new();
    let error = registry.register(plugin).await.unwrap_err();
    assert_eq!(error.code, "harness_protocol_incompatible");
    assert_eq!(registry.active_count(), 0);
}

#[tokio::test]
async fn registration_replay_requires_the_identical_profile_digest() {
    let mut registry = HarnessRegistry::new();
    registry
        .register(Arc::new(FakePlugin::events_then_success(vec![])))
        .await
        .unwrap();
    registry
        .register(Arc::new(FakePlugin::events_then_success(vec![])))
        .await
        .unwrap();

    let mut drifted = FakePlugin::events_then_success(vec![]);
    drifted.profile.program = "other-harness".to_owned();
    let error = registry.register(Arc::new(drifted)).await.unwrap_err();
    assert_eq!(error.code, "harness_protocol_incompatible");
}

#[tokio::test]
async fn cancel_targets_only_the_exact_active_invocation() {
    let plugin = Arc::new(FakePlugin::wait_for_cancel());
    let registry = registry_with(plugin).await;
    let invocation = request();
    let invocation_id = invocation.invocation_id.clone();
    let sink = Arc::new(RecordingHarnessEventSink::default());
    let task = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .invoke(invocation, Arc::new(DenyAllHarnessApprovals), sink)
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(registry.cancel(&invocation_id).await);
    assert_eq!(task.await.unwrap(), HarnessTerminal::Cancelled);
    assert!(
        !registry
            .cancel(&HarnessInvocationId::new(Uuid::now_v7()))
            .await
    );
}

#[tokio::test]
async fn registry_drains_and_sequences_events_before_returning_terminal() {
    let plugin = Arc::new(FakePlugin::events_then_success(vec![
        HarnessInvocationEvent::Started,
        HarnessInvocationEvent::Progress {
            message: "working".to_owned(),
        },
        HarnessInvocationEvent::AssistantMessage {
            text: "committed".to_owned(),
        },
    ]));
    let sink = Arc::new(RecordingHarnessEventSink::default());
    let terminal = registry_with(plugin)
        .await
        .invoke(request(), Arc::new(DenyAllHarnessApprovals), sink.clone())
        .await;
    assert!(matches!(terminal, HarnessTerminal::Completed(_)));
    assert_eq!(sink.sequences(), vec![0, 1, 2]);
}

#[test]
fn versioned_envelopes_reject_drift_and_preserve_the_canonical_digest() {
    let request_envelope = HarnessRequestEnvelope::new(request());
    let mut request_json = serde_json::to_value(request_envelope).unwrap();
    request_json["protocolVersion"] = json!(2);
    assert!(serde_json::from_value::<HarnessRequestEnvelope>(request_json).is_err());

    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    let event =
        HarnessEventEnvelope::new(invocation_id.clone(), 0, HarnessInvocationEvent::Started);
    let mut event_json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        serde_json::from_value::<HarnessEventEnvelope>(event_json.clone())
            .unwrap()
            .event_digest,
        event.event_digest
    );
    event_json["invocationId"] = json!(Uuid::now_v7());
    assert!(serde_json::from_value::<HarnessEventEnvelope>(event_json).is_err());

    let invalid_sequence =
        HarnessEventEnvelope::new(invocation_id, 1, HarnessInvocationEvent::Started);
    assert!(
        serde_json::from_value::<HarnessEventEnvelope>(
            serde_json::to_value(invalid_sequence).unwrap()
        )
        .is_err()
    );
}

#[tokio::test]
async fn sink_failure_after_the_plugin_starts_is_outcome_unknown() {
    let plugin = Arc::new(FakePlugin::events_then_success(vec![
        HarnessInvocationEvent::Started,
    ]));
    let terminal = registry_with(plugin)
        .await
        .invoke(
            request(),
            Arc::new(DenyAllHarnessApprovals),
            Arc::new(FailingHarnessEventSink),
        )
        .await;
    assert!(matches!(terminal, HarnessTerminal::OutcomeUnknown(_)));
}

#[tokio::test]
async fn default_approval_broker_rejects_every_request() {
    let decision = DenyAllHarnessApprovals
        .decide(HarnessApprovalRequest {
            invocation_id: HarnessInvocationId::new(Uuid::now_v7()),
            request_id: "approval-1".to_owned(),
            title: "Need access".to_owned(),
        })
        .await;
    assert_eq!(decision, HarnessApprovalDecision::RejectOnce);
}

#[tokio::test]
async fn duplicate_active_invocation_is_rejected_without_replacing_cancellation_ownership() {
    let registry = registry_with(Arc::new(FakePlugin::wait_for_cancel())).await;
    let invocation = request();
    let invocation_id = invocation.invocation_id.clone();
    let first = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .invoke(
                    invocation,
                    Arc::new(DenyAllHarnessApprovals),
                    Arc::new(RecordingHarnessEventSink::default()),
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    let duplicate = tokio::time::timeout(
        Duration::from_millis(100),
        registry.invoke(
            HarnessInvocationRequest {
                invocation_id: invocation_id.clone(),
                ..request()
            },
            Arc::new(DenyAllHarnessApprovals),
            Arc::new(RecordingHarnessEventSink::default()),
        ),
    )
    .await;
    assert!(matches!(
        duplicate,
        Ok(HarnessTerminal::Failed(HarnessFailure {
            code: "harness_invocation_active",
            ..
        }))
    ));
    assert!(registry.cancel(&invocation_id).await);
    assert_eq!(first.await.unwrap(), HarnessTerminal::Cancelled);
}

#[tokio::test]
async fn aborting_an_invocation_removes_its_exact_active_entry() {
    let registry = registry_with(Arc::new(FakePlugin::wait_for_cancel())).await;
    let task = tokio::spawn({
        let registry = registry.clone();
        async move {
            registry
                .invoke(
                    request(),
                    Arc::new(DenyAllHarnessApprovals),
                    Arc::new(RecordingHarnessEventSink::default()),
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(registry.active_count(), 1);
    task.abort();
    let _ = task.await;
    assert_eq!(registry.active_count(), 0);
}

#[tokio::test]
async fn completed_plugin_without_started_event_fails_closed() {
    let terminal = registry_with(Arc::new(FakePlugin::events_then_success(vec![])))
        .await
        .invoke(
            request(),
            Arc::new(DenyAllHarnessApprovals),
            Arc::new(RecordingHarnessEventSink::default()),
        )
        .await;
    assert!(matches!(
        terminal,
        HarnessTerminal::Failed(HarnessFailure {
            code: "harness_protocol_incompatible",
            ..
        })
    ));
}

#[tokio::test]
async fn sink_failure_acknowledges_every_accepted_pending_event() {
    let (plugin, mut reports) = BurstPlugin::new();
    let terminal = registry_with(plugin)
        .await
        .invoke(
            request(),
            Arc::new(DenyAllHarnessApprovals),
            Arc::new(FailAfterOneHarnessEventSink(Mutex::new(0))),
        )
        .await;
    assert!(matches!(terminal, HarnessTerminal::OutcomeUnknown(_)));
    let acknowledgements = tokio::time::timeout(Duration::from_millis(100), async {
        vec![
            reports.recv().await.unwrap(),
            reports.recv().await.unwrap(),
            reports.recv().await.unwrap(),
        ]
    })
    .await
    .expect("every accepted event receives an acknowledgement");
    assert!(acknowledgements[0].is_ok());
    for acknowledgement in acknowledgements.iter().skip(1) {
        assert!(matches!(
            acknowledgement,
            Err(HarnessFailure {
                code: "harness_event_persistence_failed",
                ..
            })
        ));
    }
}

#[tokio::test]
async fn cloned_sender_cannot_admit_events_after_terminal_drain() {
    let (plugin, release, mut reports) = LateEmitterPlugin::new();
    let sink = Arc::new(RecordingHarnessEventSink::default());
    let terminal = registry_with(plugin)
        .await
        .invoke(request(), Arc::new(DenyAllHarnessApprovals), sink.clone())
        .await;
    assert!(matches!(terminal, HarnessTerminal::Completed(_)));
    release.notify_one();
    let report = tokio::time::timeout(Duration::from_millis(100), reports.recv())
        .await
        .expect("late sender reports its rejected emission")
        .unwrap();
    assert!(matches!(
        report,
        Err(HarnessFailure {
            code: "harness_protocol_incompatible",
            ..
        })
    ));
    assert_eq!(sink.sequences(), vec![0]);
}

#[test]
fn journal_record_preserves_the_validated_envelope_digest() {
    let envelope = HarnessEventEnvelope::new(
        HarnessInvocationId::new(Uuid::now_v7()),
        0,
        HarnessInvocationEvent::Started,
    );
    let record = HarnessInvocationEventRecord::new(
        envelope.invocation_id.clone(),
        envelope.sequence,
        envelope.event.clone(),
    )
    .unwrap();
    assert_eq!(record.event_digest, envelope.event_digest);
    assert!(record.validate_digest().is_ok());
}
