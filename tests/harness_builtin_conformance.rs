#[path = "support/harness_conformance.rs"]
mod harness_conformance;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kiteframe::harness::{
    BuiltinHarnessPlugin, DenyAllHarnessApprovals, HarnessAcceptanceBarrier, HarnessCapability,
    HarnessEventEnvelope, HarnessEventSink, HarnessFailure, HarnessInvocationId,
    HarnessInvocationRequest, HarnessProfile, HarnessRegistry, HarnessRouteTable, HarnessTerminal,
    builtin_harness_profile, dispatch_with_terminal_journal,
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

const TEST_SOURCE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

struct RecordingSink(Mutex<Vec<HarnessEventEnvelope>>);

struct ObservedAcceptanceBarrier(Arc<AtomicUsize>);

struct AcceptanceAssertingModel(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl HarnessAcceptanceBarrier for ObservedAcceptanceBarrier {
    async fn commit(&self) -> Result<(), HarnessFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

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
impl ModelClient for AcceptanceAssertingModel {
    async fn respond(
        &self,
        _request: kiteframe::ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        assert_eq!(
            self.0.load(Ordering::SeqCst),
            1,
            "the durable acceptance barrier commits immediately before model dispatch"
        );
        Ok(ModelResponse {
            items: vec![json!({
                "type":"function_call",
                "name":"complete_task",
                "arguments":{"outcome":"ok","fields":{"answer":"done"}}
            })],
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
    builtin_harness_profile(TEST_SOURCE_REVISION).unwrap()
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
fn builtin_registration_requires_exact_nonzero_source_attestation() {
    assert!(builtin_harness_profile("0000000000000000000000000000000000000000").is_err());
    assert!(builtin_harness_profile("not-a-full-source-revision").is_err());
    let first = profile();
    let second = builtin_harness_profile("1123456789abcdef0123456789abcdef01234567").unwrap();
    assert_eq!(first.source_revision, TEST_SOURCE_REVISION);
    assert_eq!(
        first.id.as_str(),
        "builtin-v1-0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        second.id.as_str(),
        "builtin-v1-1123456789abcdef0123456789abcdef01234567"
    );
    assert_ne!(first.id, second.id);
    assert_ne!(first.digest().unwrap(), second.digest().unwrap());
}

#[tokio::test]
async fn builtin_revisions_coexist_and_routes_select_the_current_deployment() {
    let old = profile();
    let current = builtin_harness_profile("1123456789abcdef0123456789abcdef01234567").unwrap();
    let model = Arc::new(TestModel {
        response: json!({
            "type":"function_call",
            "name":"complete_task",
            "arguments":{"outcome":"ok","fields":{"answer":"done"}}
        }),
        delay: false,
    });
    let mut registry = HarnessRegistry::new();
    registry
        .register(Arc::new(BuiltinHarnessPlugin::new(
            old.clone(),
            model.clone(),
        )))
        .await
        .unwrap();
    registry
        .register(Arc::new(BuiltinHarnessPlugin::new(current.clone(), model)))
        .await
        .unwrap();

    assert_eq!(
        registry.profile_digest(&old.id).unwrap(),
        old.digest().unwrap()
    );
    assert_eq!(
        registry.profile_digest(&current.id).unwrap(),
        current.digest().unwrap()
    );
    let routes = HarnessRouteTable::new(current.id.clone(), current.id.clone());
    assert_eq!(routes.select(None, ModelRole::Reasoning), &current.id);
}

#[tokio::test]
async fn builtin_commits_the_injected_acceptance_barrier_before_model_dispatch() {
    let commits = Arc::new(AtomicUsize::new(0));
    let plugin = Arc::new(BuiltinHarnessPlugin::new(
        profile(),
        Arc::new(AcceptanceAssertingModel(commits.clone())),
    ));
    let mut registry = HarnessRegistry::new();
    registry.register(plugin).await.unwrap();
    let sink = Arc::new(RecordingSink(Mutex::new(vec![])));

    let terminal = registry
        .invoke_with_acceptance_barrier(
            request(),
            Arc::new(DenyAllHarnessApprovals),
            sink,
            Arc::new(ObservedAcceptanceBarrier(commits.clone())),
        )
        .await;

    assert!(matches!(terminal, HarnessTerminal::Completed(_)));
    assert_eq!(commits.load(Ordering::SeqCst), 1);
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
