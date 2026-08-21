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

enum LiveReadiness {
    Skip(&'static str),
    Ready {
        command: AcpCommand,
        environment: BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    },
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

    const fn environment_name(self) -> &'static str {
        match self {
            Self::Codex => "KITEFRAME_CODEX_ACP_ENV_JSON",
            Self::DeepSeek => "KITEFRAME_DSH_ACP_ENV_JSON",
        }
    }

    const fn required_environment_name(self) -> &'static str {
        match self {
            Self::Codex => "KITEFRAME_CODEX_ACP_REQUIRED_ENV_JSON",
            Self::DeepSeek => "KITEFRAME_DSH_ACP_REQUIRED_ENV_JSON",
        }
    }

    const fn source_revision_name(self) -> &'static str {
        match self {
            Self::Codex => "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
            Self::DeepSeek => "KITEFRAME_DSH_ACP_SOURCE_REVISION",
        }
    }

    const fn skip_line(self) -> &'static str {
        match self {
            Self::Codex => "SKIP codex live: KITEFRAME_CODEX_ACP_COMMAND_JSON is absent",
            Self::DeepSeek => "SKIP DeepSeek live: KITEFRAME_DSH_ACP_COMMAND_JSON is absent",
        }
    }

    fn readiness(
        self,
        mut value: impl FnMut(&str) -> Option<String>,
    ) -> Result<LiveReadiness, kiteframe::ConfigError> {
        let config = HarnessConfig::from_test_values(
            [
                (self.command_name(), value(self.command_name())),
                (
                    self.source_revision_name(),
                    value(self.source_revision_name()),
                ),
                (self.environment_name(), value(self.environment_name())),
                (
                    self.required_environment_name(),
                    value(self.required_environment_name()),
                ),
            ]
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (name, value))),
        )?;
        let (command, environment) = match self {
            Self::Codex => (
                config.external_harness_commands().codex,
                config.external_harness_environments().codex,
            ),
            Self::DeepSeek => (
                config.external_harness_commands().deepseek,
                config.external_harness_environments().deepseek,
            ),
        };
        let Some(command) = command else {
            return Ok(LiveReadiness::Skip(self.skip_line()));
        };
        let environment = environment
            .filter(|environment| environment.required_values_present())
            .ok_or(kiteframe::ConfigError::InvalidExternalHarnessEnvironment {
                command_name: self.command_name(),
            })?;
        Ok(LiveReadiness::Ready {
            command,
            environment: environment.values(),
        })
    }

    async fn driver_if_enabled(self) -> Option<LiveAcpDriver> {
        let readiness = self
            .readiness(|name| std::env::var(name).ok())
            .unwrap_or_else(|error| {
                panic!(
                    "{} is configured but its ACP deployment configuration is invalid: {error}",
                    self.command_name()
                )
            });
        let (command, environment) = match readiness {
            LiveReadiness::Skip(line) => {
                eprintln!("{line}");
                return None;
            }
            LiveReadiness::Ready {
                command,
                environment,
            } => (command, environment),
        };
        let profile = HarnessProfile::from_json(
            &std::fs::read(self.profile_path()).expect("checked-in profile is readable"),
        )
        .expect("checked-in profile is valid");
        Some(LiveAcpDriver {
            profile,
            command,
            environment,
        })
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
    async fn new(
        profile: HarnessProfile,
        command: AcpCommand,
        environment: BTreeMap<std::ffi::OsString, std::ffi::OsString>,
        prompt: &str,
    ) -> Self {
        let workspace = std::env::temp_dir().join(format!("kiteframe-live-acp-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).expect("live workspace is created");
        let plugin = AcpHarnessPlugin::with_environment(
            profile.clone(),
            command,
            live_limits(),
            environment,
        );
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
    environment: BTreeMap<std::ffi::OsString, std::ffi::OsString>,
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
        LiveRun::new(
            self.profile.clone(),
            self.command.clone(),
            self.environment.clone(),
            prompt,
        )
        .await
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

#[test]
fn live_target_readiness_requires_explicit_auth_and_reports_honest_skips() {
    assert!(matches!(
        LiveTarget::Codex.readiness(|_| None).unwrap(),
        LiveReadiness::Skip(line)
            if line == "SKIP codex live: KITEFRAME_CODEX_ACP_COMMAND_JSON is absent"
    ));

    assert!(
        LiveTarget::DeepSeek
            .readiness(|name| match name {
                "KITEFRAME_DSH_ACP_COMMAND_JSON" => {
                    Some(r#"["/opt/deepseek-harness-acp"]"#.to_owned())
                }
                "KITEFRAME_DSH_ACP_SOURCE_REVISION" => {
                    Some("141eb6fef83422698aef7a981029e843e8161534".to_owned())
                }
                _ => None,
            })
            .is_err(),
        "a configured target without explicit required authentication must fail"
    );

    let codex = LiveTarget::Codex
        .readiness(|name| match name {
            "KITEFRAME_CODEX_ACP_COMMAND_JSON" => Some(r#"["/absolute/codex-acp"]"#.to_owned()),
            "KITEFRAME_CODEX_ACP_SOURCE_REVISION" => {
                Some("296069e841634cd4bb9bc4515602d836e49231ec".to_owned())
            }
            "KITEFRAME_CODEX_ACP_ENV_JSON" => {
                Some(r#"{"OPENAI_API_KEY":"placeholder-not-a-secret"}"#.to_owned())
            }
            "KITEFRAME_CODEX_ACP_REQUIRED_ENV_JSON" => Some(r#"["OPENAI_API_KEY"]"#.to_owned()),
            _ => None,
        })
        .unwrap();
    let LiveReadiness::Ready {
        command,
        environment,
    } = codex
    else {
        panic!("complete documented Codex configuration must be ready");
    };
    assert_eq!(command.program, PathBuf::from("/absolute/codex-acp"));
    assert!(command.args.is_empty());
    assert!(environment.contains_key(&std::ffi::OsString::from("OPENAI_API_KEY")));

    let directory =
        std::env::temp_dir().join(format!("kiteframe-live-readiness-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&directory).unwrap();
    let pnpm = directory.join("pnpm");
    std::fs::write(&pnpm, "placeholder executable").unwrap();
    let expected_pnpm = std::fs::canonicalize(&pnpm).unwrap();
    let environment = serde_json::json!({
        "DEEPSEEK_API_KEY": "placeholder-not-a-secret",
        "PATH": directory.to_string_lossy(),
    })
    .to_string();
    let deepseek = LiveTarget::DeepSeek.readiness(|name| match name {
        "KITEFRAME_DSH_ACP_COMMAND_JSON" => {
            Some(r#"["pnpm","--dir","/absolute/deepseek-harness","run","demo:acp"]"#.to_owned())
        }
        "KITEFRAME_DSH_ACP_SOURCE_REVISION" => {
            Some("141eb6fef83422698aef7a981029e843e8161534".to_owned())
        }
        "KITEFRAME_DSH_ACP_ENV_JSON" => Some(environment.clone()),
        "KITEFRAME_DSH_ACP_REQUIRED_ENV_JSON" => Some(r#"["DEEPSEEK_API_KEY"]"#.to_owned()),
        _ => None,
    });
    std::fs::remove_dir_all(&directory).unwrap();
    let deepseek = deepseek.unwrap();
    let LiveReadiness::Ready {
        command,
        environment,
    } = deepseek
    else {
        panic!("complete documented DeepSeek configuration must be ready");
    };
    assert_eq!(command.program, expected_pnpm);
    assert_eq!(
        command.args,
        vec![
            std::ffi::OsString::from("--dir"),
            std::ffi::OsString::from("/absolute/deepseek-harness"),
            std::ffi::OsString::from("run"),
            std::ffi::OsString::from("demo:acp"),
        ]
    );
    assert!(environment.contains_key(&std::ffi::OsString::from("DEEPSEEK_API_KEY")));
    assert!(environment.contains_key(&std::ffi::OsString::from("PATH")));
}

#[test]
fn live_target_readiness_rejects_partial_configuration_without_echoing_values() {
    let cases = [
        (
            "command only",
            vec![("KITEFRAME_CODEX_ACP_COMMAND_JSON", r#"["/opt/codex-acp"]"#)],
        ),
        (
            "command and revision without live auth",
            vec![
                ("KITEFRAME_CODEX_ACP_COMMAND_JSON", r#"["/opt/codex-acp"]"#),
                (
                    "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                    "296069e841634cd4bb9bc4515602d836e49231ec",
                ),
            ],
        ),
        (
            "environment without required keys",
            vec![
                ("KITEFRAME_CODEX_ACP_COMMAND_JSON", r#"["/opt/codex-acp"]"#),
                (
                    "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                    "296069e841634cd4bb9bc4515602d836e49231ec",
                ),
                (
                    "KITEFRAME_CODEX_ACP_ENV_JSON",
                    r#"{"OPENAI_API_KEY":"private-sentinel"}"#,
                ),
            ],
        ),
        (
            "revision only",
            vec![(
                "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                "296069e841634cd4bb9bc4515602d836e49231ec",
            )],
        ),
        (
            "environment only",
            vec![(
                "KITEFRAME_CODEX_ACP_ENV_JSON",
                r#"{"OPENAI_API_KEY":"private-sentinel"}"#,
            )],
        ),
        (
            "required keys only",
            vec![(
                "KITEFRAME_CODEX_ACP_REQUIRED_ENV_JSON",
                r#"["OPENAI_API_KEY"]"#,
            )],
        ),
        (
            "malformed command",
            vec![
                ("KITEFRAME_CODEX_ACP_COMMAND_JSON", "not-json"),
                (
                    "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                    "296069e841634cd4bb9bc4515602d836e49231ec",
                ),
            ],
        ),
        (
            "blank command",
            vec![
                ("KITEFRAME_CODEX_ACP_COMMAND_JSON", ""),
                (
                    "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                    "296069e841634cd4bb9bc4515602d836e49231ec",
                ),
            ],
        ),
    ];

    for (case, values) in cases {
        let error = LiveTarget::Codex
            .readiness(|name| {
                values
                    .iter()
                    .find(|(configured_name, _)| *configured_name == name)
                    .map(|(_, value)| (*value).to_owned())
            })
            .err()
            .unwrap_or_else(|| panic!("{case} must not become a live skip"));
        assert!(
            !error.to_string().contains("private-sentinel"),
            "{case} exposed an environment value"
        );
    }
}
