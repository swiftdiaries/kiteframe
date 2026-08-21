#[path = "support/harness_conformance.rs"]
mod harness_conformance;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kiteframe::harness::{
    BuiltinHarnessPlugin, DenyAllHarnessApprovals, HarnessCapability, HarnessEventEnvelope,
    HarnessEventSink, HarnessFailure, HarnessInvocationId, HarnessInvocationRequest,
    HarnessProfile, HarnessProfileId, HarnessRegistry, HarnessTerminal, HarnessTransport,
    dispatch_with_terminal_journal,
};
use kiteframe::{
    AgentCommandId, AgentKernel, ClosedSchema, GoalPhase, GoalProjection, HarnessPlugin,
    HarnessStructuralLimits, HarnessTool, InvokeHarness, ModelClient, ModelError, ModelResponse,
    ModelRole, PrimitiveValue,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use harness_conformance::{
    HarnessConformanceCase, HarnessConformanceDriver, HarnessConformanceObservation,
    RecordingTerminalJournal, assert_normative_harness,
};

struct BuiltinConformanceDriver;

struct RecordingSink(Mutex<Vec<HarnessEventEnvelope>>);

#[async_trait::async_trait]
impl HarnessEventSink for RecordingSink {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        self.0.lock().await.push(event);
        Ok(())
    }
}

struct TestModel {
    response: Value,
    delay: bool,
}

struct CountingTestModel {
    response: Value,
    calls: AtomicUsize,
}

impl CountingTestModel {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ModelClient for CountingTestModel {
    async fn respond(
        &self,
        _request: kiteframe::ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse {
            items: vec![self.response.clone()],
        })
    }
}

#[async_trait::async_trait]
impl ModelClient for TestModel {
    async fn respond(
        &self,
        _request: kiteframe::ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        if self.delay {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        Ok(ModelResponse {
            items: vec![self.response.clone()],
        })
    }
}

impl BuiltinConformanceDriver {
    fn new() -> Self {
        Self
    }
}

fn profile() -> HarnessProfile {
    HarnessProfile {
        id: HarnessProfileId::new("builtin-v1").unwrap(),
        transport: HarnessTransport::Builtin,
        source_repository: "https://github.com/swiftdiaries/kiteframe".to_owned(),
        source_revision: "0000000000000000000000000000000000000000".to_owned(),
        program: "kiteframe-builtin".to_owned(),
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

#[tokio::test]
async fn builtin_profile_advertises_and_uses_permission_once() {
    let plugin = BuiltinHarnessPlugin::new(
        profile(),
        Arc::new(TestModel {
            response: json!({
                "type": "function_call",
                "name": "docs/search",
                "arguments": {},
            }),
            delay: false,
        }),
    );
    assert!(
        plugin
            .profile()
            .required
            .contains(&HarnessCapability::PermissionOnce)
    );
}

#[test]
fn builtin_v1_registration_digest_remains_upgrade_compatible() {
    assert_eq!(
        profile().digest().unwrap(),
        "c5f8f5116ca8e338fc5e764e89e72aee0f657da519eb3e8f97395bc0a6373e5b",
        "builtin-v1 registration must replay with its pre-fix profile digest"
    );
}

fn permission_request() -> HarnessInvocationRequest {
    let mut invocation = request();
    invocation.payload.tools.push(HarnessTool {
        qualified_name: "kiteframe/request_permission".to_owned(),
        descriptor_digest: "sha256:permission-request".to_owned(),
        descriptor: json!({"type":"permission_once"}),
    });
    invocation
}

fn request() -> HarnessInvocationRequest {
    HarnessInvocationRequest {
        invocation_id: HarnessInvocationId::new(uuid::Uuid::now_v7()),
        command_id: AgentCommandId::new(uuid::Uuid::now_v7()),
        command_revision: 0,
        profile_id: profile().id,
        workspace: "/workspace".into(),
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
                active_execution_millis_remaining: 30_000,
            },
        },
    }
}

async fn invoke(
    response: Value,
    delay: bool,
) -> (
    Arc<HarnessRegistry>,
    HarnessInvocationRequest,
    HarnessTerminal,
    u64,
) {
    let plugin = Arc::new(BuiltinHarnessPlugin::new(
        profile(),
        Arc::new(TestModel { response, delay }),
    ));
    let mut registry = HarnessRegistry::new();
    registry.register(plugin).await.unwrap();
    let registry = Arc::new(registry);
    let request = request();
    let sink = Arc::new(RecordingSink(Mutex::new(vec![])));
    let terminal = registry
        .invoke(
            request.clone(),
            Arc::new(DenyAllHarnessApprovals),
            sink.clone(),
        )
        .await;
    let count = sink.0.lock().await.len() as u64;
    (registry, request, terminal, count)
}

#[async_trait::async_trait]
impl HarnessConformanceDriver for BuiltinConformanceDriver {
    async fn run(&mut self, case: HarnessConformanceCase) -> HarnessConformanceObservation {
        let (terminal, terminal_count) = match case {
            HarnessConformanceCase::Complete => {
                let result = invoke(json!({"type":"function_call","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}}), false).await;
                (result.2, result.3)
            }
            HarnessConformanceCase::Replay => {
                let model = Arc::new(CountingTestModel {
                    response: json!({"type":"function_call","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}}),
                    calls: AtomicUsize::new(0),
                });
                let plugin = Arc::new(BuiltinHarnessPlugin::new(profile(), model.clone()));
                let mut registry = HarnessRegistry::new();
                registry.register(plugin).await.unwrap();
                let request = request();
                let invocation_id = request.invocation_id.clone();
                let journal = RecordingTerminalJournal::default();
                let sink = Arc::new(RecordingSink(Mutex::new(vec![])));
                let first = dispatch_with_terminal_journal(
                    &journal,
                    &invocation_id,
                    |terminal| async move { Ok(terminal) },
                    || async {
                        let terminal = registry
                            .invoke(
                                request.clone(),
                                Arc::new(DenyAllHarnessApprovals),
                                sink.clone(),
                            )
                            .await;
                        journal
                            .record(invocation_id.clone(), terminal.clone())
                            .await;
                        Ok(terminal)
                    },
                )
                .await
                .unwrap();
                let second = dispatch_with_terminal_journal(
                    &journal,
                    &invocation_id,
                    |terminal| async move { Ok(terminal) },
                    || async {
                        let terminal = registry
                            .invoke(request, Arc::new(DenyAllHarnessApprovals), sink.clone())
                            .await;
                        journal
                            .record(invocation_id.clone(), terminal.clone())
                            .await;
                        Ok(terminal)
                    },
                )
                .await
                .unwrap();
                assert_eq!(first, second);
                assert_eq!(model.calls(), 1, "terminal replay must not call the model");
                (second, journal.terminal_count().await)
            }
            HarnessConformanceCase::RejectPermission => {
                let plugin = Arc::new(BuiltinHarnessPlugin::new(
                    profile(),
                    Arc::new(TestModel {
                        response: json!({"type":"function_call","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"unused"}}}),
                        delay: false,
                    }),
                ));
                let mut registry = HarnessRegistry::new();
                registry.register(plugin).await.unwrap();
                let sink = Arc::new(RecordingSink(Mutex::new(vec![])));
                let terminal = registry
                    .invoke(
                        permission_request(),
                        Arc::new(DenyAllHarnessApprovals),
                        sink.clone(),
                    )
                    .await;
                let events = sink.0.lock().await;
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
                                decision: kiteframe::harness::HarnessApprovalDecision::RejectOnce,
                                ..
                            },
                            ..
                        }
                    ]
                ));
                (terminal, events.len() as u64)
            }
            HarnessConformanceCase::InvalidCompletion => {
                let result = invoke(json!({"type":"function_call","name":"complete_task","arguments":{"outcome":"ok","fields":{}}}), false).await;
                (result.2, result.3)
            }
            HarnessConformanceCase::CancelInFlight => {
                let plugin = Arc::new(BuiltinHarnessPlugin::new(
                    profile(),
                    Arc::new(TestModel {
                        response: json!({}),
                        delay: true,
                    }),
                ));
                let mut registry = HarnessRegistry::new();
                registry.register(plugin).await.unwrap();
                let registry = Arc::new(registry);
                let request = request();
                let id = request.invocation_id.clone();
                let sink = Arc::new(RecordingSink(Mutex::new(vec![])));
                let task = tokio::spawn({
                    let registry = registry.clone();
                    let sink = sink.clone();
                    async move {
                        registry
                            .invoke(request, Arc::new(DenyAllHarnessApprovals), sink)
                            .await
                    }
                });
                tokio::time::sleep(Duration::from_millis(10)).await;
                assert!(registry.cancel(&id).await);
                let terminal = task.await.unwrap();
                (terminal, sink.0.lock().await.len() as u64)
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
async fn builtin_passes_the_normative_v1_suite() {
    let _constructor: fn(HarnessProfile, Arc<dyn ModelClient>) -> BuiltinHarnessPlugin =
        BuiltinHarnessPlugin::new;
    let _dispatcher = AgentKernel::with_harness_registry;
    let _registry = HarnessRegistry::new();
    assert_normative_harness(BuiltinConformanceDriver::new()).await;
}
