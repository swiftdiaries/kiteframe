#[path = "support/harness_conformance.rs"]
mod harness_conformance;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_conformance::{
    HarnessConformanceCase, HarnessConformanceDriver, HarnessConformanceObservation,
    RecordingTerminalJournal, assert_normative_harness,
};
use kiteframe::harness::{
    AcpCommand, AcpHarnessPlugin, AcpLimits, AcpStdioClient, AcpTestBarrier, AcpTestBarrierPoint,
    HarnessAcceptanceBarrier, HarnessApprovalBroker, HarnessApprovalDecision,
    HarnessApprovalRequest, HarnessCapability, HarnessEventEnvelope, HarnessEventSink,
    HarnessFailure, HarnessInvocationId, HarnessInvocationRequest, HarnessProfile,
    HarnessProfileId, HarnessRegistry, HarnessTerminal, HarnessTransport,
    dispatch_with_terminal_journal, select_permission_once,
};
use kiteframe::{
    AgentCommandId, ClosedSchema, GoalPhase, GoalProjection, HarnessStructuralLimits,
    InvokeHarness, ModelRole, PrimitiveValue,
};
use serde_json::{Value, json};
use tokio::sync::Notify;
use uuid::Uuid;

const SOURCE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Default)]
struct RecordingSink(Mutex<Vec<HarnessEventEnvelope>>);

#[async_trait]
impl HarnessEventSink for RecordingSink {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

struct TestAcceptanceBarrier;

#[async_trait]
impl HarnessAcceptanceBarrier for TestAcceptanceBarrier {
    async fn commit(&self) -> Result<(), HarnessFailure> {
        Ok(())
    }
}

struct CancelDuringAcceptance {
    registry: Arc<HarnessRegistry>,
    invocation_id: HarnessInvocationId,
}

#[async_trait]
impl HarnessAcceptanceBarrier for CancelDuringAcceptance {
    async fn commit(&self) -> Result<(), HarnessFailure> {
        assert!(self.registry.cancel(&self.invocation_id).await);
        Ok(())
    }
}

struct FixedApproval(HarnessApprovalDecision);

#[async_trait]
impl HarnessApprovalBroker for FixedApproval {
    async fn decide(&self, _request: HarnessApprovalRequest) -> HarnessApprovalDecision {
        self.0.clone()
    }
}

struct BlockingApproval {
    entered: Arc<Notify>,
}

struct CountingApproval {
    calls: Arc<AtomicUsize>,
    decision: HarnessApprovalDecision,
}

#[async_trait]
impl HarnessApprovalBroker for CountingApproval {
    async fn decide(&self, _request: HarnessApprovalRequest) -> HarnessApprovalDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.decision.clone()
    }
}

#[derive(Default)]
struct GatedMessageSink {
    events: Mutex<Vec<HarnessEventEnvelope>>,
    message_entered: Notify,
    release_message: Notify,
}

#[async_trait]
impl HarnessEventSink for GatedMessageSink {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        let is_message = matches!(
            event.event,
            kiteframe::harness::HarnessInvocationEvent::AssistantMessage { .. }
        );
        self.events.lock().unwrap().push(event);
        if is_message {
            self.message_entered.notify_one();
            self.release_message.notified().await;
        }
        Ok(())
    }
}

#[async_trait]
impl HarnessApprovalBroker for BlockingApproval {
    async fn decide(&self, _request: HarnessApprovalRequest) -> HarnessApprovalDecision {
        self.entered.notify_one();
        std::future::pending().await
    }
}

struct FixtureRun {
    workspace: PathBuf,
    observation: PathBuf,
    registry: Arc<HarnessRegistry>,
    request: HarnessInvocationRequest,
    sink: Arc<RecordingSink>,
}

impl FixtureRun {
    async fn new(script: impl AsRef<Path>) -> Self {
        Self::new_inner(script, None).await
    }

    async fn new_with_barrier(script: impl AsRef<Path>, barrier: Arc<AcpTestBarrier>) -> Self {
        Self::new_inner(script, Some(barrier)).await
    }

    async fn new_inner(script: impl AsRef<Path>, barrier: Option<Arc<AcpTestBarrier>>) -> Self {
        let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        let observation = workspace.join("observation.json");
        let profile = profile();
        let mut plugin = AcpHarnessPlugin::with_environment(
            profile.clone(),
            AcpCommand {
                program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
                args: Vec::<OsString>::new(),
                source_revision: SOURCE_REVISION.to_owned(),
            },
            limits(),
            BTreeMap::from([
                (
                    OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                    script.as_ref().as_os_str().to_owned(),
                ),
                (
                    OsString::from("KITEFRAME_FAKE_ACP_OBSERVATION"),
                    observation.as_os_str().to_owned(),
                ),
                (
                    OsString::from("KITEFRAME_FAKE_ACP_ALLOW_REGISTRATION_EOF"),
                    OsString::from("1"),
                ),
            ]),
        );
        if let Some(barrier) = barrier {
            plugin = plugin.with_test_barrier(barrier);
        }
        let mut registry = HarnessRegistry::new();
        let pre_accept_attempt_limit = plugin.pre_accept_attempt_limit();
        registry
            .register_with_pre_accept_attempt_limit(Arc::new(plugin), pre_accept_attempt_limit)
            .await
            .unwrap();
        let registry = Arc::new(registry);
        let request = request(profile.id, workspace.clone());
        Self {
            workspace,
            observation,
            registry,
            request,
            sink: Arc::new(RecordingSink::default()),
        }
    }

    async fn invoke(&self, decision: HarnessApprovalDecision) -> HarnessTerminal {
        self.registry
            .invoke_with_acceptance_barrier(
                self.request.clone(),
                Arc::new(FixedApproval(decision)),
                self.sink.clone(),
                Arc::new(TestAcceptanceBarrier),
            )
            .await
    }

    fn observation(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.observation).unwrap()).unwrap()
    }

    async fn wait_for_method(&self, wanted: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            if self.observation.exists()
                && self.observation()["methods"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|method| method == wanted)
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "fake ACP did not observe {wanted}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

impl Drop for FixtureRun {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct AcpConformanceDriver;

impl AcpConformanceDriver {
    fn fixture(_name: &str) -> Self {
        Self
    }
}

#[async_trait]
impl HarnessConformanceDriver for AcpConformanceDriver {
    async fn run(&mut self, case: HarnessConformanceCase) -> HarnessConformanceObservation {
        let (terminal, terminal_count) = match case {
            HarnessConformanceCase::Complete => {
                let run = FixtureRun::new(fixture("core-success.json")).await;
                let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
                (terminal, 1)
            }
            HarnessConformanceCase::Replay => {
                let run = FixtureRun::new(fixture("core-success.json")).await;
                let journal = RecordingTerminalJournal::default();
                let id = run.request.invocation_id.clone();
                let first = dispatch_with_terminal_journal(
                    &journal,
                    &id,
                    |terminal| async { Ok(terminal) },
                    || async {
                        let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
                        journal.record(id.clone(), terminal.clone()).await;
                        Ok(terminal)
                    },
                )
                .await
                .unwrap();
                let second = dispatch_with_terminal_journal(
                    &journal,
                    &id,
                    |terminal| async { Ok(terminal) },
                    || async { panic!("terminal replay must not launch another ACP process") },
                )
                .await
                .unwrap();
                assert_eq!(first, second);
                (second, journal.terminal_count().await)
            }
            HarnessConformanceCase::RejectPermission => {
                let run = FixtureRun::new(fixture("permission-once.json")).await;
                let terminal = run.invoke(HarnessApprovalDecision::RejectOnce).await;
                (terminal, 1)
            }
            HarnessConformanceCase::CancelInFlight => {
                let run = Arc::new(FixtureRun::new(fixture("cancel.json")).await);
                let id = run.request.invocation_id.clone();
                let task = tokio::spawn({
                    let run = run.clone();
                    async move { run.invoke(HarnessApprovalDecision::AllowOnce).await }
                });
                run.wait_for_method("session/prompt").await;
                assert!(run.registry.cancel(&id).await);
                let terminal = task.await.unwrap();
                assert_eq!(run.observation()["cancelCount"], 1);
                (terminal, 1)
            }
            HarnessConformanceCase::InvalidCompletion => {
                let script = script_with_completion(json!({"outcome":"ok","fields":{}}));
                let run = FixtureRun::new(write_script(&script)).await;
                let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
                (terminal, 1)
            }
        };
        HarnessConformanceObservation {
            terminal,
            terminal_count,
            goal_phase: GoalPhase::Active,
        }
    }
}

#[tokio::test]
async fn scripted_acp_passes_the_same_normative_suite() {
    assert_normative_harness(AcpConformanceDriver::fixture("core-success.json")).await;
}

#[tokio::test]
async fn permission_answer_is_one_shot_and_correlated() {
    let run = FixtureRun::new(fixture("permission-once.json")).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert!(
        matches!(terminal, HarnessTerminal::Completed(_)),
        "unexpected terminal: {terminal:?}"
    );
    let observation = run.observation();
    assert_eq!(observation["scriptComplete"], true);
    assert_eq!(observation["selectedPermissionOption"], "allow-once-id");
    assert_eq!(observation["permissionResponseCount"], 1);
    let environment = observation["environmentKeys"].as_array().unwrap();
    assert!(environment.iter().all(|key| key != "HOME" && key != "PATH"));
    assert!(
        environment
            .iter()
            .all(|key| !key.as_str().unwrap().contains("TOKEN"))
    );
    assert_eq!(observation["promptBlockCount"], 1);
    let prompt = observation["promptText"].as_str().unwrap();
    assert!(prompt.contains("Treat the complete structured goal"));
    assert!(prompt.contains("\"objective\":\"test\""));
    assert!(prompt.contains("\"resultSchema\""));
}

#[tokio::test]
async fn reject_once_is_correlated_and_durably_audited() {
    let run = FixtureRun::new(fixture("permission-once.json")).await;
    let terminal = run.invoke(HarnessApprovalDecision::RejectOnce).await;
    assert_eq!(
        terminal_code(&terminal),
        Some("harness_permission_rejected")
    );
    let observation = run.observation();
    assert_eq!(observation["selectedPermissionOption"], "reject-once-id");
    assert_eq!(observation["permissionResponseCount"], 1);
    let events = run.sink.0.lock().unwrap();
    assert!(matches!(
        events.as_slice(),
        [
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::Started,
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalRequested { .. },
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalResolved {
                    decision: HarnessApprovalDecision::RejectOnce,
                    ..
                },
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn cancellation_resolves_a_pending_permission_and_sends_cancel_once() {
    let mut script: Value =
        serde_json::from_slice(&std::fs::read(fixture("permission-once.json")).unwrap()).unwrap();
    script[3]["allowCancelBeforeResponse"] = json!(true);
    let run = Arc::new(FixtureRun::new(write_script(&script)).await);
    let entered = Arc::new(Notify::new());
    let task = tokio::spawn({
        let run = run.clone();
        let entered = entered.clone();
        async move {
            run.registry
                .invoke_with_acceptance_barrier(
                    run.request.clone(),
                    Arc::new(BlockingApproval { entered }),
                    run.sink.clone(),
                    Arc::new(TestAcceptanceBarrier),
                )
                .await
        }
    });
    entered.notified().await;
    assert!(run.registry.cancel(&run.request.invocation_id).await);
    assert_eq!(task.await.unwrap(), HarnessTerminal::Cancelled);
    let observation = run.observation();
    assert_eq!(observation["cancelCount"], 1);
    assert_eq!(observation["permissionResponseCount"], 1);
    assert!(observation["selectedPermissionOption"].is_null());
    let events = run.sink.0.lock().unwrap();
    assert!(matches!(
        events.as_slice(),
        [
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::Started,
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalRequested { .. },
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalResolved {
                    decision: HarnessApprovalDecision::RejectOnce,
                    ..
                },
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn cancellation_committed_with_acceptance_prevents_prompt_write() {
    let script = json!([
        {
            "expect":{"method":"initialize"},
            "respond":stable_initialize()
        },
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}}
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let terminal = run
        .registry
        .invoke_with_acceptance_barrier(
            run.request.clone(),
            Arc::new(FixedApproval(HarnessApprovalDecision::AllowOnce)),
            run.sink.clone(),
            Arc::new(CancelDuringAcceptance {
                registry: run.registry.clone(),
                invocation_id: run.request.invocation_id.clone(),
            }),
        )
        .await;
    assert_eq!(terminal, HarnessTerminal::Cancelled);
    let observation = run.observation();
    assert_eq!(observation["cancelCount"], 0);
    assert_eq!(observation["mismatchedFrame"], Value::Null);
    assert_eq!(observation["trailingFrame"], Value::Null);
    assert!(
        observation["methods"]
            .as_array()
            .unwrap()
            .iter()
            .all(|method| method != "session/prompt")
    );
}

#[tokio::test]
async fn cancellation_after_prompt_admission_but_before_write_drops_it_permanently() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/cancel"}}
    ]);
    let barrier = Arc::new(AcpTestBarrier::new(
        "session/prompt",
        AcpTestBarrierPoint::BeforeWrite,
    ));
    let run = Arc::new(FixtureRun::new_with_barrier(write_script(&script), barrier.clone()).await);
    let invocation_id = run.request.invocation_id.clone();
    let task = tokio::spawn({
        let run = run.clone();
        async move { run.invoke(HarnessApprovalDecision::AllowOnce).await }
    });
    barrier.wait_until_entered().await;
    assert!(run.registry.cancel(&invocation_id).await);
    barrier.release();
    let terminal = task.await.unwrap();
    assert_eq!(terminal, HarnessTerminal::Cancelled);
    let observation = run.observation();
    assert_eq!(observation["cancelCount"], 0);
    assert_eq!(observation["mismatchedFrame"], Value::Null);
    assert_eq!(observation["trailingFrame"], Value::Null);
    assert!(
        observation["methods"]
            .as_array()
            .unwrap()
            .iter()
            .all(|method| method != "session/prompt")
    );
}

#[tokio::test]
async fn cancellation_racing_prompt_write_ack_cancels_a_possibly_sent_prompt_once() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {"expect":{"method":"session/cancel"}},
        {"respondTo":"prompt","respond":{"stopReason":"cancelled"}}
    ]);
    let barrier = Arc::new(AcpTestBarrier::new(
        "session/prompt",
        AcpTestBarrierPoint::BeforeAcknowledgement,
    ));
    let run = Arc::new(FixtureRun::new_with_barrier(write_script(&script), barrier.clone()).await);
    let invocation_id = run.request.invocation_id.clone();
    let task = tokio::spawn({
        let run = run.clone();
        async move { run.invoke(HarnessApprovalDecision::AllowOnce).await }
    });
    barrier.wait_until_entered().await;
    assert!(run.registry.cancel(&invocation_id).await);
    barrier.release();
    let terminal = task.await.unwrap();
    assert_eq!(terminal, HarnessTerminal::Cancelled);
    let observation = run.observation();
    assert_eq!(observation["cancelCount"], 1);
    assert_eq!(observation["mismatchedFrame"], Value::Null);
    assert_eq!(observation["trailingFrame"], Value::Null);
    assert_eq!(
        observation["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|method| *method == "session/prompt")
            .count(),
        1
    );
}

#[tokio::test]
async fn simultaneous_cancellation_wins_over_ready_end_turn() {
    let run = FixtureRun::new(fixture("core-success.json")).await;
    let sink = Arc::new(GatedMessageSink::default());
    let task = tokio::spawn({
        let registry = run.registry.clone();
        let request = run.request.clone();
        let sink = sink.clone();
        async move {
            registry
                .invoke_with_acceptance_barrier(
                    request,
                    Arc::new(FixedApproval(HarnessApprovalDecision::AllowOnce)),
                    sink,
                    Arc::new(TestAcceptanceBarrier),
                )
                .await
        }
    });
    sink.message_entered.notified().await;
    assert!(run.registry.cancel(&run.request.invocation_id).await);
    sink.release_message.notify_one();
    assert_eq!(task.await.unwrap(), HarnessTerminal::Cancelled);
}

#[tokio::test]
async fn permission_arriving_after_cancellation_is_rejected_without_second_cancel() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {"expect":{"method":"session/cancel","params":{"sessionId":"session-1"}}},
        {
            "request":permission_request(json!(700), true),
            "allowCancelBeforeResponse":true,
            "expectResponse":{"id":700}
        },
        {"respondTo":"prompt","respond":{"stopReason":"cancelled"}}
    ]);
    let run = Arc::new(FixtureRun::new(write_script(&script)).await);
    let calls = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let run = run.clone();
        let calls = calls.clone();
        async move {
            run.registry
                .invoke_with_acceptance_barrier(
                    run.request.clone(),
                    Arc::new(CountingApproval {
                        calls,
                        decision: HarnessApprovalDecision::AllowOnce,
                    }),
                    run.sink.clone(),
                    Arc::new(TestAcceptanceBarrier),
                )
                .await
        }
    });
    run.wait_for_method("session/prompt").await;
    assert!(run.registry.cancel(&run.request.invocation_id).await);
    assert_eq!(task.await.unwrap(), HarnessTerminal::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let observation = run.observation();
    assert_eq!(observation["cancelCount"], 1);
    assert_eq!(observation["permissionResponseCount"], 1);
    let events = run.sink.0.lock().unwrap();
    assert!(matches!(
        &events[events.len() - 2..],
        [
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalRequested { .. },
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalResolved {
                    decision: HarnessApprovalDecision::RejectOnce,
                    ..
                },
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn duplicate_permission_request_id_is_rejected_before_second_broker_call() {
    let duplicate = permission_request(json!(700), true);
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {"request":duplicate,"expectResponse":{"id":700}},
        {"request":permission_request(json!(700), true),"expectResponse":{"id":700}}
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let terminal = run
        .registry
        .invoke_with_acceptance_barrier(
            run.request.clone(),
            Arc::new(CountingApproval {
                calls: calls.clone(),
                decision: HarnessApprovalDecision::AllowOnce,
            }),
            run.sink.clone(),
            Arc::new(TestAcceptanceBarrier),
        )
        .await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.observation()["permissionResponseCount"], 1);
}

#[tokio::test]
async fn numeric_and_string_permission_ids_are_distinct_and_each_answered_once() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {"request":permission_request(json!(7), true),"expectResponse":{"id":7}},
        {"request":permission_request(json!("7"), true),"expectResponse":{"id":"7"}},
        completion_notification(),
        {"respondTo":"prompt","respond":{"stopReason":"end_turn"}}
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let terminal = run
        .registry
        .invoke_with_acceptance_barrier(
            run.request.clone(),
            Arc::new(CountingApproval {
                calls: calls.clone(),
                decision: HarnessApprovalDecision::AllowOnce,
            }),
            run.sink.clone(),
            Arc::new(TestAcceptanceBarrier),
        )
        .await;
    assert!(matches!(terminal, HarnessTerminal::Completed(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(run.observation()["permissionResponseCount"], 2);
    let request_ids = run
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match &event.event {
            kiteframe::harness::HarnessInvocationEvent::ApprovalRequested {
                request_id, ..
            } => Some(request_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(request_ids, ["number:7", "string:7"]);
    let resolved_ids = run
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match &event.event {
            kiteframe::harness::HarnessInvocationEvent::ApprovalResolved { request_id, .. } => {
                Some(request_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved_ids, ["number:7", "string:7"]);
}

#[tokio::test]
async fn negative_and_null_permission_ids_match_the_acp_request_id_domain() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {"request":permission_request(json!(-7), true),"expectResponse":{"id":-7}},
        {"request":permission_request(Value::Null, true),"expectResponse":{"id":null}},
        completion_notification(),
        {"respondTo":"prompt","respond":{"stopReason":"end_turn"}}
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert!(matches!(terminal, HarnessTerminal::Completed(_)));
    assert_eq!(run.observation()["permissionResponseCount"], 2);
    let request_ids = run
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match &event.event {
            kiteframe::harness::HarnessInvocationEvent::ApprovalRequested {
                request_id, ..
            } => Some(request_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(request_ids, ["number:-7", "null"]);
    let resolved_ids = run
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match &event.event {
            kiteframe::harness::HarnessInvocationEvent::ApprovalResolved { request_id, .. } => {
                Some(request_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved_ids, ["number:-7", "null"]);
}

#[tokio::test]
async fn missing_one_shot_option_gets_one_error_after_durable_resolution() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {
            "request":permission_request(json!(701), false),
            "expectResponse":{"id":701,"error":{"code":-32001}}
        }
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(
        terminal_code(&terminal),
        Some("harness_capability_unsupported")
    );
    assert_eq!(run.observation()["permissionResponseCount"], 1);
    let events = run.sink.0.lock().unwrap();
    assert!(matches!(
        &events[events.len() - 2..],
        [
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalRequested { .. },
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalResolved {
                    decision: HarnessApprovalDecision::AllowOnce,
                    ..
                },
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn malformed_permission_request_gets_reject_resolution_and_one_error() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {
            "request":{
                "id":703,
                "method":"session/request_permission",
                "params":{"sessionId":"session-1","options":[]}
            },
            "expectResponse":{"id":703,"error":{"code":-32602}}
        }
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let terminal = run
        .registry
        .invoke_with_acceptance_barrier(
            run.request.clone(),
            Arc::new(CountingApproval {
                calls: calls.clone(),
                decision: HarnessApprovalDecision::AllowOnce,
            }),
            run.sink.clone(),
            Arc::new(TestAcceptanceBarrier),
        )
        .await;
    assert_eq!(
        terminal_code(&terminal),
        Some("harness_protocol_incompatible")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(run.observation()["permissionResponseCount"], 1);
    let events = run.sink.0.lock().unwrap();
    assert!(matches!(
        &events[events.len() - 2..],
        [
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalRequested { .. },
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalResolved {
                    decision: HarnessApprovalDecision::RejectOnce,
                    ..
                },
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn blocking_approval_is_bounded_by_the_active_execution_deadline() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {
            "request":permission_request(json!(702), true),
            "expectResponse":{"id":702,"error":{"code":-32002}}
        }
    ]);
    let mut run = FixtureRun::new(write_script(&script)).await;
    run.request.payload.limits.active_execution_millis_remaining = 200;
    let entered = Arc::new(Notify::new());
    let terminal = tokio::time::timeout(
        Duration::from_secs(1),
        run.registry.invoke_with_acceptance_barrier(
            run.request.clone(),
            Arc::new(BlockingApproval {
                entered: entered.clone(),
            }),
            run.sink.clone(),
            Arc::new(TestAcceptanceBarrier),
        ),
    )
    .await
    .expect("approval broker must not outlive active execution");
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
    assert_eq!(run.observation()["permissionResponseCount"], 1);
    let events = run.sink.0.lock().unwrap();
    assert!(matches!(
        &events[events.len() - 2..],
        [
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalRequested { .. },
                ..
            },
            HarnessEventEnvelope {
                event: kiteframe::harness::HarnessInvocationEvent::ApprovalResolved {
                    decision: HarnessApprovalDecision::RejectOnce,
                    ..
                },
                ..
            }
        ]
    ));
}

#[tokio::test]
async fn assistant_bytes_are_bounded_before_text_or_event_append() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {
            "notify":{
                "method":"session/update",
                "params":{
                    "sessionId":"session-1",
                    "update":{
                        "sessionUpdate":"agent_message_chunk",
                        "content":{"type":"text","text":"too many bytes"}
                    }
                }
            }
        },
        {"respondTo":"prompt","respond":{"stopReason":"end_turn"}}
    ]);
    let mut run = FixtureRun::new(write_script(&script)).await;
    run.request.payload.limits.response_item_bytes = 8;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_failed"));
    assert!(run.sink.0.lock().unwrap().iter().all(|event| !matches!(
        event.event,
        kiteframe::harness::HarnessInvocationEvent::AssistantMessage { .. }
    )));
}

#[tokio::test]
async fn assistant_update_count_is_closed_before_the_overflow_event() {
    let mut script = vec![
        json!({"expect":{"method":"initialize"},"respond":stable_initialize()}),
        json!({"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}}),
        json!({"expect":{"method":"session/prompt"},"saveId":"prompt"}),
    ];
    for _ in 0..65 {
        script.push(json!({
            "notify":{
                "method":"session/update",
                "params":{
                    "sessionId":"session-1",
                    "update":{
                        "sessionUpdate":"agent_message_chunk",
                        "content":{"type":"text","text":""}
                    }
                }
            }
        }));
    }
    script.push(json!({"respondTo":"prompt","respond":{"stopReason":"end_turn"}}));
    let run = FixtureRun::new(write_script(&Value::Array(script))).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_failed"));
    let message_count = run
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(
                event.event,
                kiteframe::harness::HarnessInvocationEvent::AssistantMessage { .. }
            )
        })
        .count();
    assert_eq!(message_count, 64);
}

#[tokio::test]
async fn dropped_public_requests_reclaim_pending_capacity() {
    let mut script = (0..32)
        .map(|_| json!({"expect":{"method":"probe"}}))
        .collect::<Vec<_>>();
    script.push(json!({"expect":{"method":"probe"},"respond":{"ok":true}}));
    let script = write_script(&Value::Array(script));
    let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let observation = workspace.join("observation.json");
    let client = Arc::new(
        AcpStdioClient::spawn(
            &AcpCommand {
                program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
                args: vec![],
                source_revision: SOURCE_REVISION.to_owned(),
            },
            &workspace,
            limits(),
            &BTreeMap::from([
                (
                    OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                    script.as_os_str().to_owned(),
                ),
                (
                    OsString::from("KITEFRAME_FAKE_ACP_OBSERVATION"),
                    observation.as_os_str().to_owned(),
                ),
            ]),
        )
        .await
        .unwrap(),
    );
    let mut requests = Vec::new();
    for index in 0..32 {
        let client = client.clone();
        requests.push(tokio::spawn(async move {
            client
                .request::<_, Value>("probe", json!({"index":index}))
                .await
        }));
    }
    wait_for_observed_method_count(&observation, 32).await;
    for request in requests {
        request.abort();
        let _ = request.await;
    }
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client.request::<_, Value>("probe", json!({"final":true})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response, json!({"ok":true}));
    let _ = client.shutdown().await;
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn request_admission_racing_connection_failure_is_rejected_without_waiting() {
    let script = write_script(&json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"sleepMillis":100},
        {"rawStdout":"not-json-rpc"},
        {"sleepMillis":5000}
    ]));
    let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let barrier = Arc::new(AcpTestBarrier::new(
        "probe",
        AcpTestBarrierPoint::BeforeAdmission,
    ));
    let client = Arc::new(
        AcpStdioClient::spawn_with_test_barrier(
            &AcpCommand {
                program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
                args: vec![],
                source_revision: SOURCE_REVISION.to_owned(),
            },
            &workspace,
            limits(),
            &BTreeMap::from([(
                OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                script.as_os_str().to_owned(),
            )]),
            barrier.clone(),
        )
        .await
        .unwrap(),
    );
    client.initialize().await.unwrap();
    let request = tokio::spawn({
        let client = client.clone();
        async move { client.request::<_, Value>("probe", json!({})).await }
    });
    barrier.wait_until_entered().await;
    let connection_error = client.recv().await.unwrap_err();
    barrier.release();
    let request_error = tokio::time::timeout(Duration::from_millis(250), request)
        .await
        .expect("request admitted after connection failure must not wait")
        .unwrap()
        .unwrap_err();
    assert_eq!(request_error, connection_error);
    client.kill().await;
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn eof_after_prompt_acceptance_is_outcome_unknown() {
    let run = FixtureRun::new(fixture("process-death.json")).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
}

#[tokio::test]
async fn completed_requires_zero_status_graceful_shutdown() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        completion_notification(),
        {"respondTo":"prompt","respond":{"stopReason":"end_turn"}},
        {"exitCode":17}
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
}

#[tokio::test]
async fn completed_rejects_premature_zero_status_eof() {
    let release = std::env::temp_dir().join(format!("kiteframe-acp-release-{}", Uuid::now_v7()));
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        completion_notification(),
        {"respondTo":"prompt","respond":{"stopReason":"end_turn"}},
        {"waitForFile":release.to_str().unwrap()},
        {"exitCode":0}
    ]);
    let barrier = Arc::new(AcpTestBarrier::new_disarmed(
        "stdout/eof",
        AcpTestBarrierPoint::EofBeforeShutdownClaim,
    ));
    let run = Arc::new(FixtureRun::new_with_barrier(write_script(&script), barrier.clone()).await);
    barrier.arm();
    let invocation = tokio::spawn({
        let run = run.clone();
        async move { run.invoke(HarnessApprovalDecision::AllowOnce).await }
    });
    barrier.wait_until_preclaim_entered().await;
    wait_for_observation_field(&run.observation, "waitingForFile").await;
    std::fs::write(&release, b"exit").unwrap();
    barrier.wait_until_entered().await;
    barrier.release_preclaim();
    barrier.wait_until_shutdown_claim_attempted().await;
    barrier.release();
    let terminal = invocation.await.unwrap();
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
    let _ = std::fs::remove_file(release);
}

#[tokio::test]
async fn fake_records_mismatched_and_trailing_frames_before_failing() {
    let mismatched = run_unexpected_fake_frame(json!([{"expect":{"method":"initialize"}}])).await;
    assert_eq!(mismatched["mismatchedFrame"]["method"], "probe");
    assert_eq!(mismatched["trailingFrame"], Value::Null);

    let trailing = run_unexpected_fake_frame(json!([])).await;
    assert_eq!(trailing["mismatchedFrame"], Value::Null);
    assert_eq!(trailing["trailingFrame"]["method"], "probe");
}

#[tokio::test]
async fn completed_rejects_an_incomplete_fake_script() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        completion_notification(),
        {"respondTo":"prompt","respond":{"stopReason":"end_turn"}},
        {"expect":{"method":"session/cancel"}}
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
    assert_eq!(run.observation()["scriptComplete"], false);
}

#[tokio::test]
async fn completed_requires_shutdown_to_finish_within_grace() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        completion_notification(),
        {"respondTo":"prompt","respond":{"stopReason":"end_turn"}},
        {"sleepMillis":5000}
    ]);
    let run = FixtureRun::new(write_script(&script)).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
}

#[tokio::test]
async fn protocol_mismatch_fails_before_prompt_acceptance() {
    let error = registration_error(fixture("incompatible.json")).await;
    assert_eq!(error.code, "harness_protocol_incompatible");
}

#[tokio::test]
async fn registration_rejects_agent_that_exits_after_initialize() {
    let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let observation = workspace.join("observation.json");
    let release = workspace.join("release-exit");
    let script = json!([
        {
            "expect":{"method":"initialize"},
            "respond":stable_initialize()
        },
        {"waitForFile":release.to_str().unwrap()},
        {"exitCode":0}
    ]);
    let script = write_script(&script);
    let barrier = Arc::new(AcpTestBarrier::new(
        "stdout/eof",
        AcpTestBarrierPoint::EofBeforeShutdownClaim,
    ));
    let plugin = AcpHarnessPlugin::with_environment(
        profile(),
        AcpCommand {
            program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
            args: vec![],
            source_revision: SOURCE_REVISION.to_owned(),
        },
        limits(),
        BTreeMap::from([
            (
                OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                script.as_os_str().to_owned(),
            ),
            (
                OsString::from("KITEFRAME_FAKE_ACP_OBSERVATION"),
                observation.as_os_str().to_owned(),
            ),
        ]),
    )
    .with_test_barrier(barrier.clone());
    let registration = tokio::spawn(async move {
        let mut registry = HarnessRegistry::new();
        registry.register(Arc::new(plugin)).await
    });
    barrier.wait_until_preclaim_entered().await;
    wait_for_observation_field(&observation, "waitingForFile").await;
    std::fs::write(&release, b"exit").unwrap();
    barrier.wait_until_entered().await;
    barrier.release_preclaim();
    barrier.wait_until_shutdown_claim_attempted().await;
    barrier.release();
    let error = registration.await.unwrap().unwrap_err();
    assert_eq!(error.code, "harness_profile_unavailable");
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn registration_rejects_nonzero_shutdown_status() {
    let script = json!([
        {"expect":{"method":"initialize"},"respond":stable_initialize()},
        {"expect":{"method":"session/new"}}
    ]);
    let error = registration_error(write_script(&script)).await;
    assert_eq!(error.code, "harness_profile_unavailable");
}

#[tokio::test]
async fn shutdown_timeout_kills_child_and_releases_blocked_writes() {
    let script = write_script(&json!([{"sleepMillis":5000}]));
    let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let client = Arc::new(
        AcpStdioClient::spawn(
            &AcpCommand {
                program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
                args: vec![],
                source_revision: SOURCE_REVISION.to_owned(),
            },
            &workspace,
            AcpLimits {
                max_frame_bytes: 64 * 1024,
                shutdown_grace: Duration::from_millis(50),
                max_pre_accept_attempts: 1,
            },
            &BTreeMap::from([(
                OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                script.as_os_str().to_owned(),
            )]),
        )
        .await
        .unwrap(),
    );
    let payload = "x".repeat(60 * 1024);
    let mut writes = Vec::new();
    for _ in 0..32 {
        let client = client.clone();
        let payload = payload.clone();
        writes.push(tokio::spawn(async move {
            client
                .request::<_, Value>("probe", json!({"payload":payload}))
                .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    let started = tokio::time::Instant::now();
    assert!(client.shutdown().await.is_err());
    assert!(started.elapsed() < Duration::from_millis(500));
    for write in writes {
        let _ = tokio::time::timeout(Duration::from_millis(500), write)
            .await
            .expect("blocked writer task must be joined or released");
    }
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn eof_before_prompt_acceptance_is_retryable() {
    let script = json!([{
        "expect":{"method":"initialize"},
        "exitAfterRead":19
    }]);
    let mutable_script = write_script(
        &serde_json::from_slice(&std::fs::read(fixture("core-success.json")).unwrap()).unwrap(),
    );
    let run = FixtureRun::new(&mutable_script).await;
    std::fs::write(&mutable_script, serde_json::to_vec(&script).unwrap()).unwrap();
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert!(matches!(
        terminal,
        HarnessTerminal::Retryable(HarnessFailure {
            code: "harness_failed",
            ..
        })
    ));
}

#[tokio::test]
async fn malformed_stdout_fails_closed_before_acceptance() {
    let script = json!([{"rawStdout":"this is not json"}]);
    let error = registration_error(write_script(&script)).await;
    assert_eq!(error.code, "harness_protocol_incompatible");
}

#[tokio::test]
async fn duplicate_response_is_protocol_incompatible() {
    let script = json!([
        {
            "expect":{"method":"initialize"},
            "respond":{
                "protocolVersion":1,
                "agentCapabilities":{},
                "agentInfo":{"name":"kiteframe-fake-acp","version":"1.0.0"}
            },
            "duplicateResponse":true
        },
        {"sleepMillis":100}
    ]);
    let error = registration_error(write_script(&script)).await;
    assert_eq!(error.code, "harness_protocol_incompatible");
}

#[tokio::test]
async fn active_execution_timeout_after_acceptance_is_outcome_unknown() {
    let script = json!([
        {
            "expect":{"method":"initialize"},
            "respond":{
                "protocolVersion":1,
                "agentCapabilities":{},
                "agentInfo":{"name":"kiteframe-fake-acp","version":"1.0.0"}
            }
        },
        {"expect":{"method":"session/new"},"respond":{"sessionId":"session-1"}},
        {"expect":{"method":"session/prompt"},"saveId":"prompt"},
        {"sleepMillis":1000}
    ]);
    let mut run = FixtureRun::new(write_script(&script)).await;
    run.request.payload.limits.active_execution_millis_remaining = 200;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
}

#[tokio::test]
async fn unnegotiated_rich_required_capability_is_rejected_at_registration() {
    let mut rich_profile = profile();
    rich_profile.required.insert(HarnessCapability::SessionLoad);
    let plugin = AcpHarnessPlugin::new(
        rich_profile,
        AcpCommand {
            program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
            args: vec![],
            source_revision: SOURCE_REVISION.to_owned(),
        },
        limits(),
    );
    let mut registry = HarnessRegistry::new();
    let error = registry.register(Arc::new(plugin)).await.unwrap_err();
    assert_eq!(error.code, "harness_capability_unsupported");
}

#[tokio::test]
async fn strict_completion_rejects_unknown_top_level_fields() {
    let script = script_with_completion(json!({
        "outcome":"ok",
        "fields":{"answer":"done"},
        "extra":true
    }));
    let run = FixtureRun::new(write_script(&script)).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_failed"));
}

#[test]
fn permission_selection_never_promotes_a_one_shot_decision() {
    let options = vec![
        kiteframe::harness::PermissionOption {
            id: "always".to_owned(),
            kind: "allow_always".to_owned(),
        },
        kiteframe::harness::PermissionOption {
            id: "once".to_owned(),
            kind: "allow_once".to_owned(),
        },
    ];
    assert_eq!(
        select_permission_once(&options, HarnessApprovalDecision::AllowOnce).unwrap(),
        "once"
    );
    assert_eq!(
        select_permission_once(&options, HarnessApprovalDecision::RejectOnce)
            .unwrap_err()
            .code,
        "harness_capability_unsupported"
    );
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acp")
        .join(name)
}

fn write_script(value: &Value) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kiteframe-acp-script-{}.json", Uuid::now_v7()));
    std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

async fn wait_for_observed_method_count(path: &Path, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let observed = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["methods"].as_array().map(Vec::len))
            .unwrap_or(0);
        if observed >= count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fake ACP observed {observed} methods, expected at least {count}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn wait_for_observation_field(path: &Path, field: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| !value[field].is_null())
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fake ACP did not record observation field {field}"
        );
        tokio::task::yield_now().await;
    }
}

async fn run_unexpected_fake_frame(script: Value) -> Value {
    let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let observation = workspace.join("observation.json");
    let script = write_script(&script);
    let client = AcpStdioClient::spawn(
        &AcpCommand {
            program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
            args: vec![],
            source_revision: SOURCE_REVISION.to_owned(),
        },
        &workspace,
        limits(),
        &BTreeMap::from([
            (
                OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                script.as_os_str().to_owned(),
            ),
            (
                OsString::from("KITEFRAME_FAKE_ACP_OBSERVATION"),
                observation.as_os_str().to_owned(),
            ),
        ]),
    )
    .await
    .unwrap();
    assert!(
        client
            .request::<_, Value>("probe", json!({}))
            .await
            .is_err()
    );
    client.kill().await;
    let observed = std::fs::read(&observation)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| json!({}));
    let _ = std::fs::remove_dir_all(workspace);
    observed
}

async fn registration_error(script: impl AsRef<Path>) -> HarnessFailure {
    let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).unwrap();
    let plugin = AcpHarnessPlugin::with_environment(
        profile(),
        AcpCommand {
            program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
            args: vec![],
            source_revision: SOURCE_REVISION.to_owned(),
        },
        limits(),
        BTreeMap::from([(
            OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
            script.as_ref().as_os_str().to_owned(),
        )]),
    );
    let mut registry = HarnessRegistry::new();
    let error = registry.register(Arc::new(plugin)).await.unwrap_err();
    let _ = std::fs::remove_dir_all(workspace);
    error
}

fn script_with_completion(completion: Value) -> Value {
    let mut script: Value =
        serde_json::from_slice(&std::fs::read(fixture("core-success.json")).unwrap()).unwrap();
    script[3]["notify"]["params"]["update"]["content"]["text"] =
        Value::String(serde_json::to_string(&completion).unwrap());
    script
}

fn stable_initialize() -> Value {
    json!({
        "protocolVersion":1,
        "agentCapabilities":{},
        "agentInfo":{"name":"kiteframe-fake-acp","version":"1.0.0"}
    })
}

fn permission_request(id: Value, include_one_shot: bool) -> Value {
    let mut options = vec![json!({
        "optionId":"allow-always-id",
        "name":"Always",
        "kind":"allow_always"
    })];
    if include_one_shot {
        options.extend([
            json!({"optionId":"allow-once-id","name":"Once","kind":"allow_once"}),
            json!({"optionId":"reject-once-id","name":"Reject","kind":"reject_once"}),
        ]);
    }
    json!({
        "id":id,
        "method":"session/request_permission",
        "params":{
            "sessionId":"session-1",
            "toolCall":{"toolCallId":"tool-1","title":"Run fixture tool"},
            "options":options
        }
    })
}

fn completion_notification() -> Value {
    json!({
        "notify":{
            "method":"session/update",
            "params":{
                "sessionId":"session-1",
                "update":{
                    "sessionUpdate":"agent_message_chunk",
                    "content":{
                        "type":"text",
                        "text":"{\"fields\":{\"answer\":\"done\"},\"outcome\":\"ok\"}"
                    }
                }
            }
        }
    })
}

fn terminal_code(terminal: &HarnessTerminal) -> Option<&str> {
    match terminal {
        HarnessTerminal::Completed(_) => None,
        HarnessTerminal::Cancelled => Some("harness_cancelled"),
        HarnessTerminal::Retryable(error)
        | HarnessTerminal::Failed(error)
        | HarnessTerminal::OutcomeUnknown(error) => Some(error.code),
    }
}

fn profile() -> HarnessProfile {
    HarnessProfile {
        id: HarnessProfileId::new("fake-acp-v1").unwrap(),
        transport: HarnessTransport::AcpStdio,
        source_repository: "https://example.test/fake-acp.git".to_owned(),
        source_revision: SOURCE_REVISION.to_owned(),
        program: "kiteframe-fake-acp".to_owned(),
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

fn limits() -> AcpLimits {
    AcpLimits {
        max_frame_bytes: 64 * 1024,
        shutdown_grace: Duration::from_millis(250),
        max_pre_accept_attempts: 1,
    }
}

fn request(profile_id: HarnessProfileId, workspace: PathBuf) -> HarnessInvocationRequest {
    HarnessInvocationRequest {
        invocation_id: HarnessInvocationId::new(Uuid::now_v7()),
        command_id: AgentCommandId::new(Uuid::now_v7()),
        command_revision: 0,
        profile_id,
        workspace,
        payload_digest: "sha256:test".to_owned(),
        payload: InvokeHarness {
            role: ModelRole::Reasoning,
            prompt: "finish".to_owned(),
            goal: GoalProjection {
                objective: "test".to_owned(),
                success: vec![],
                constraints: vec![],
                blocked_when: vec![],
            },
            input: json!({}),
            input_schema: ClosedSchema::object("input", BTreeMap::new()),
            result_schema: ClosedSchema::object(
                "result",
                BTreeMap::from([
                    (
                        "outcome".to_owned(),
                        ClosedSchema::enumeration(BTreeSet::from([PrimitiveValue::String(
                            "ok".to_owned(),
                        )])),
                    ),
                    (
                        "fields".to_owned(),
                        ClosedSchema::object(
                            "outcomes",
                            BTreeMap::from([(
                                "ok".to_owned(),
                                ClosedSchema::object(
                                    "ok_fields",
                                    BTreeMap::from([("answer".to_owned(), ClosedSchema::string())]),
                                ),
                            )]),
                        ),
                    ),
                ]),
            ),
            tools: vec![],
            projected_history: vec![],
            limits: HarnessStructuralLimits {
                model_turns_remaining: 1,
                tool_calls_per_response: 1,
                response_item_bytes: 4096,
                active_execution_millis_remaining: 5_000,
            },
        },
    }
}
