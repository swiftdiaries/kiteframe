use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiteframe::harness::{
    DenyAllHarnessApprovals, HarnessApprovalBroker, HarnessApprovalDecision,
    HarnessApprovalRequest, HarnessCapabilities, HarnessCapability, HarnessEventEnvelope,
    HarnessEventSender, HarnessEventSink, HarnessFailure, HarnessInvocationEvent,
    HarnessInvocationId, HarnessInvocationOutput, HarnessInvocationRequest, HarnessPlugin,
    HarnessProfile, HarnessProfileId, HarnessRegistry, HarnessRequestEnvelope, HarnessTerminal,
    HarnessTransport, emit_and_wait,
};
use kiteframe::{
    AgentCommandId, ClosedSchema, GoalProjection, HarnessStructuralLimits, InvokeHarness, ModelRole,
};
use serde_json::json;
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

enum FakeMode {
    Events(Vec<HarnessInvocationEvent>),
    WaitForCancel,
}

struct FakePlugin {
    profile: HarnessProfile,
    protocol_version: u16,
    mode: FakeMode,
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
