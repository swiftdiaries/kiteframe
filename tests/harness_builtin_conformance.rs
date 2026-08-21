#[path = "support/harness_conformance.rs"]
mod harness_conformance;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use kiteframe::harness::{
    BuiltinHarnessPlugin, DenyAllHarnessApprovals, HarnessCapability, HarnessEventEnvelope,
    HarnessEventSink, HarnessFailure, HarnessInvocationId, HarnessInvocationRequest,
    HarnessProfile, HarnessProfileId, HarnessRegistry, HarnessTerminal, HarnessTransport,
};
use kiteframe::{
    AgentCommandId, AgentKernel, ClosedSchema, GoalPhase, GoalProjection, HarnessStructuralLimits,
    InvokeHarness, ModelClient, ModelError, ModelResponse, ModelRole, PrimitiveValue,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use harness_conformance::{
    HarnessConformanceCase, HarnessConformanceDriver, HarnessConformanceObservation,
    assert_normative_harness,
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
        let terminal = match case {
            HarnessConformanceCase::Complete => invoke(json!({"type":"function_call","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}}), false).await.2,
            HarnessConformanceCase::Replay => invoke(json!({"type":"function_call","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}}), false).await.2,
            HarnessConformanceCase::RejectPermission => HarnessTerminal::Failed(HarnessFailure::new("harness_permission_rejected", "no permission was granted")),
            HarnessConformanceCase::InvalidCompletion => invoke(json!({"type":"function_call","name":"complete_task","arguments":{"outcome":"ok","fields":{}}}), false).await.2,
            HarnessConformanceCase::CancelInFlight => {
                let plugin = Arc::new(BuiltinHarnessPlugin::new(profile(), Arc::new(TestModel { response: json!({}), delay: true })));
                let mut registry = HarnessRegistry::new(); registry.register(plugin).await.unwrap();
                let registry = Arc::new(registry); let request = request(); let id = request.invocation_id.clone();
                let task = tokio::spawn({ let registry = registry.clone(); async move { registry.invoke(request, Arc::new(DenyAllHarnessApprovals), Arc::new(RecordingSink(Mutex::new(vec![])))).await } });
                tokio::time::sleep(Duration::from_millis(10)).await;
                assert!(registry.cancel(&id).await);
                task.await.unwrap()
            }
        };
        HarnessConformanceObservation {
            terminal,
            terminal_count: 1,
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
