#[path = "support/harness_conformance.rs"]
mod harness_conformance;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_conformance::{
    HarnessConformanceCase, HarnessConformanceDriver, HarnessConformanceObservation,
    RecordingTerminalJournal, assert_normative_harness,
};
use kiteframe::harness::{
    AcpCommand, AcpHarnessPlugin, AcpLimits, HarnessAcceptanceBarrier, HarnessApprovalBroker,
    HarnessApprovalDecision, HarnessApprovalRequest, HarnessCapability, HarnessEventEnvelope,
    HarnessEventSink, HarnessFailure, HarnessInvocationId, HarnessInvocationRequest,
    HarnessProfile, HarnessProfileId, HarnessRegistry, HarnessTerminal, HarnessTransport,
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
        let workspace = std::env::temp_dir().join(format!("kiteframe-acp-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        let observation = workspace.join("observation.json");
        let profile = profile();
        let plugin = AcpHarnessPlugin::with_environment(
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
            ]),
        );
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
}

#[tokio::test]
async fn eof_after_prompt_acceptance_is_outcome_unknown() {
    let run = FixtureRun::new(fixture("process-death.json")).await;
    let terminal = run.invoke(HarnessApprovalDecision::AllowOnce).await;
    assert_eq!(terminal_code(&terminal), Some("harness_outcome_unknown"));
}

#[tokio::test]
async fn protocol_mismatch_fails_before_prompt_acceptance() {
    let error = registration_error(fixture("incompatible.json")).await;
    assert_eq!(error.code, "harness_protocol_incompatible");
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
