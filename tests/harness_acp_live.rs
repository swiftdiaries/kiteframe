#![allow(dead_code)]

#[path = "support/harness_conformance.rs"]
mod harness_conformance;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_conformance::{
    HarnessConformanceCase, HarnessConformanceDriver, HarnessConformanceObservation,
    RecordingTerminalJournal, assert_normative_harness,
};
use kiteframe::harness::{
    AcpCommand, AcpHarnessPlugin, AcpLimits, HarnessAcceptanceBarrier, HarnessApprovalBroker,
    HarnessApprovalDecision, HarnessApprovalRequest, HarnessEventEnvelope, HarnessEventSink,
    HarnessFailure, HarnessInvocationId, HarnessInvocationRequest, HarnessProfile, HarnessRegistry,
    HarnessTerminal, dispatch_with_terminal_journal,
};
use kiteframe::{
    AgentCommandId, ClosedSchema, GoalPhase, GoalProjection, HarnessConfig,
    HarnessStructuralLimits, InvokeHarness, ModelRole, PrimitiveValue,
};
use serde_json::json;
use uuid::Uuid;

const CODEX_PROFILE: &str = "config/harness-profiles/codex-acp-v1.json";
const DEEPSEEK_PROFILE: &str = "config/harness-profiles/deepseek-harness-acp-v1.json";

#[derive(Clone, Copy)]
enum LiveTarget {
    Codex,
    DeepSeek,
}

impl LiveTarget {
    const fn command_name(self) -> &'static str {
        match self {
            Self::Codex => "KITEFRAME_CODEX_ACP_COMMAND_JSON",
            Self::DeepSeek => "KITEFRAME_DSH_ACP_COMMAND_JSON",
        }
    }

    const fn profile_path(self) -> &'static str {
        match self {
            Self::Codex => CODEX_PROFILE,
            Self::DeepSeek => DEEPSEEK_PROFILE,
        }
    }

    const fn skip_line(self) -> &'static str {
        match self {
            Self::Codex => "SKIP codex live: KITEFRAME_CODEX_ACP_COMMAND_JSON is absent",
            Self::DeepSeek => "SKIP DeepSeek live: KITEFRAME_DSH_ACP_COMMAND_JSON is absent",
        }
    }

    async fn enabled() -> Vec<Self> {
        [Self::Codex, Self::DeepSeek]
            .into_iter()
            .filter(|target| std::env::var_os(target.command_name()).is_some())
            .collect()
    }

    async fn driver_if_enabled(self) -> Option<LiveAcpDriver> {
        if std::env::var_os(self.command_name()).is_none() {
            eprintln!("{}", self.skip_line());
            return None;
        }
        let config = HarnessConfig::from_env().unwrap_or_else(|error| {
            panic!(
                "{} is configured but the deployment configuration is invalid: {error}",
                self.command_name()
            )
        });
        let command = match self {
            Self::Codex => config.external_harness_commands().codex,
            Self::DeepSeek => config.external_harness_commands().deepseek,
        }
        .expect("command presence was checked before parsing live configuration");
        let profile = HarnessProfile::from_json(
            &std::fs::read(self.profile_path()).expect("checked-in profile is readable"),
        )
        .expect("checked-in profile is valid");
        Some(LiveAcpDriver { profile, command })
    }
}

#[derive(Default)]
struct RecordingSink(Mutex<Vec<HarnessEventEnvelope>>);

#[async_trait]
impl HarnessEventSink for RecordingSink {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        self.0
            .lock()
            .expect("event sink lock is not poisoned")
            .push(event);
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

struct LiveRun {
    workspace: PathBuf,
    registry: Arc<HarnessRegistry>,
    request: HarnessInvocationRequest,
    sink: Arc<RecordingSink>,
}

impl LiveRun {
    async fn new(profile: HarnessProfile, command: AcpCommand, prompt: &str) -> Self {
        let workspace = std::env::temp_dir().join(format!("kiteframe-live-acp-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).expect("live workspace is created");
        let plugin = AcpHarnessPlugin::new(profile.clone(), command, live_limits());
        let pre_accept_attempt_limit = plugin.pre_accept_attempt_limit();
        let mut registry = HarnessRegistry::new();
        registry
            .register_with_pre_accept_attempt_limit(Arc::new(plugin), pre_accept_attempt_limit)
            .await
            .expect("live ACP profile registers before its normative cases run");
        Self {
            request: request(profile.id, workspace.clone(), prompt),
            workspace,
            registry: Arc::new(registry),
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
}

impl Drop for LiveRun {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

struct LiveAcpDriver {
    profile: HarnessProfile,
    command: AcpCommand,
}

#[async_trait]
impl HarnessConformanceDriver for LiveAcpDriver {
    async fn run(&mut self, case: HarnessConformanceCase) -> HarnessConformanceObservation {
        let (terminal, terminal_count) = match case {
            HarnessConformanceCase::Complete => {
                let run = self.new_run("Return exactly {\"outcome\":\"ok\",\"fields\":{\"answer\":\"done\"}}. Do not use tools or modify files.").await;
                (run.invoke(HarnessApprovalDecision::AllowOnce).await, 1)
            }
            HarnessConformanceCase::Replay => {
                let run = self.new_run("Return exactly {\"outcome\":\"ok\",\"fields\":{\"answer\":\"done\"}}. Do not use tools or modify files.").await;
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
                .expect("live terminal is journaled");
                let second = dispatch_with_terminal_journal(
                    &journal,
                    &id,
                    |terminal| async { Ok(terminal) },
                    || async { panic!("terminal replay must not launch another ACP process") },
                )
                .await
                .expect("live terminal replay is stable");
                assert_eq!(first, second);
                (second, journal.terminal_count().await)
            }
            HarnessConformanceCase::RejectPermission => {
                let run = self.new_run("Use a harmless no-op operation that asks for permission once, then return exactly {\"outcome\":\"ok\",\"fields\":{\"answer\":\"done\"}}.").await;
                (run.invoke(HarnessApprovalDecision::RejectOnce).await, 1)
            }
            HarnessConformanceCase::CancelInFlight => {
                let run = Arc::new(
                    self.new_run("Wait for a long time before responding; do not use tools.")
                        .await,
                );
                let invocation_id = run.request.invocation_id.clone();
                let task = tokio::spawn({
                    let run = run.clone();
                    async move { run.invoke(HarnessApprovalDecision::AllowOnce).await }
                });
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert!(run.registry.cancel(&invocation_id).await);
                (task.await.expect("live invocation task joins"), 1)
            }
            HarnessConformanceCase::InvalidCompletion => {
                let run = self
                    .new_run("Reply with exactly the words not JSON. Do not use tools.")
                    .await;
                (run.invoke(HarnessApprovalDecision::AllowOnce).await, 1)
            }
        };
        HarnessConformanceObservation {
            terminal,
            terminal_count,
            goal_phase: GoalPhase::Active,
        }
    }
}

impl LiveAcpDriver {
    async fn new_run(&self, prompt: &str) -> LiveRun {
        LiveRun::new(self.profile.clone(), self.command.clone(), prompt).await
    }
}

fn live_limits() -> AcpLimits {
    AcpLimits {
        max_frame_bytes: 1024 * 1024,
        shutdown_grace: Duration::from_secs(5),
        max_pre_accept_attempts: 3,
    }
}

fn request(
    profile_id: kiteframe::harness::HarnessProfileId,
    workspace: PathBuf,
    prompt: &str,
) -> HarnessInvocationRequest {
    HarnessInvocationRequest {
        invocation_id: HarnessInvocationId::new(Uuid::now_v7()),
        command_id: AgentCommandId::new(Uuid::now_v7()),
        command_revision: 0,
        profile_id,
        workspace,
        payload_digest: "sha256:live-acp".to_owned(),
        payload: InvokeHarness {
            role: ModelRole::Coding,
            prompt: prompt.to_owned(),
            goal: GoalProjection {
                objective: "ACP live portable-core certification".to_owned(),
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
                active_execution_millis_remaining: 15_000,
            },
        },
    }
}

#[tokio::test]
async fn live_codex_and_deepseek_use_the_same_normative_cases() {
    for target in LiveTarget::enabled().await {
        assert_normative_harness(
            target
                .driver_if_enabled()
                .await
                .expect("enabled target has a live driver"),
        )
        .await;
    }
}

#[tokio::test]
async fn codex_acp_live_core() {
    let Some(driver) = LiveTarget::Codex.driver_if_enabled().await else {
        return;
    };
    assert_normative_harness(driver).await;
}

#[tokio::test]
async fn deepseek_harness_acp_live_core() {
    let Some(driver) = LiveTarget::DeepSeek.driver_if_enabled().await else {
        return;
    };
    assert_normative_harness(driver).await;
}
