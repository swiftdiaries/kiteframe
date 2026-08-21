use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiteframe::dsl::{Limits, compile};
use kiteframe::harness::{
    DenyAllHarnessApprovals, HarnessInvocationId, HarnessProfile, HarnessRegistry,
    builtin_harness_profile,
};
use kiteframe::store::{
    HarnessInvocationState, HarnessPluginRegistration, PrepareHarnessInvocation,
};
use kiteframe::{
    AgentCommandDispatcher, AgentCommandId, AgentCommandView, AgentExecutionView, AgentKernel,
    AgentSelector, BuiltinHarnessPlugin, CancelUserTask, CatalogTool, ClaimAgentCommand,
    ClosedSchema, CompleteAgentCommand, CreateSession, EffectKind, EvidenceId, EvidenceRecord,
    EvidenceSourceIdentity, GoalContract, GoalPhase, HarnessConfig, HarnessDispatcherTestBarrier,
    HarnessDispatcherTestBarrierPoint, HarnessDispatcherTestFault, HarnessDispatcherTestFaultPoint,
    HarnessExecutionError, IdempotencyKey, InvokeHarness, KernelCommandPayload, ModelClient,
    ModelError, ModelRequest, ModelResponse, OperationalStatus, PauseUserTask, PostgresStore,
    PrincipalId, RecoverySupport, RequestContext, RuntimeCeilings, RuntimeEvent, StartAgent,
    StartRun, StoreError, SubmitUserTask, SuccessCriterion, TaskLifecycle, TaskOwnerAuthorizer,
    TaskPreferences, TaskTarget, ToolCatalogSnapshot, UserTaskId, WaitForEvent, canonical_sha256,
};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

type ModelFuture = Pin<Box<dyn Future<Output = Result<ModelResponse, ModelError>> + Send>>;
type ModelHandler = dyn Fn(ModelRequest) -> ModelFuture + Send + Sync;
static BRIDGE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static LAST_BRIDGE_DATABASE: Mutex<Option<IsolatedDatabase>> = Mutex::new(None);

#[derive(Clone)]
struct IsolatedDatabase {
    name: String,
    admin_url: String,
    url: String,
}

impl IsolatedDatabase {
    async fn fresh(base_url: &str) -> Self {
        let previous = LAST_BRIDGE_DATABASE.lock().unwrap().take();
        if let Some(previous) = previous {
            previous.drop().await;
        }
        let mut admin = url::Url::parse(base_url).unwrap();
        admin.set_path("/postgres");
        let name = format!("kiteframe_task7_bridge_{}", Uuid::now_v7().simple());
        let admin_url = admin.to_string();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&pool)
            .await
            .unwrap();
        drop(pool);
        admin.set_path(&format!("/{name}"));
        let database = Self {
            name,
            admin_url,
            url: admin.to_string(),
        };
        *LAST_BRIDGE_DATABASE.lock().unwrap() = Some(database.clone());
        database
    }

    async fn drop(self) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .unwrap();
        sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
            .execute(&pool)
            .await
            .unwrap();
    }
}

async fn force_legacy_terminal_journal(
    pool: &PgPool,
    invocation_id: &HarnessInvocationId,
    state: &str,
    terminal_key: &str,
    payload: Value,
) {
    sqlx::query(
        "UPDATE harness_invocations SET state = $1, terminal = TRUE, \
         terminal_request_digest = $2, terminal_payload = $3 WHERE invocation_id = $4",
    )
    .bind(state)
    .bind(terminal_key)
    .bind(payload)
    .bind(invocation_id.as_uuid())
    .execute(pool)
    .await
    .unwrap();
}

fn builtin_profile() -> HarnessProfile {
    builtin_harness_profile("0123456789abcdef0123456789abcdef01234567").unwrap()
}

async fn non_command_fingerprint(pool: &PgPool, task_id: &UserTaskId) -> Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object( \
             'task', (SELECT to_jsonb(t) FROM user_tasks t WHERE t.id = $1), \
             'goalContract', (SELECT to_jsonb(g) FROM goal_contracts g WHERE g.task_id = $1), \
             'goalState', (SELECT to_jsonb(g) FROM goal_states g WHERE g.task_id = $1), \
             'snapshot', (SELECT to_jsonb(s) FROM agent_runtime_snapshots s WHERE s.task_id = $1), \
             'transitions', (SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.id), '[]'::jsonb) \
                 FROM agent_transitions t WHERE t.task_id = $1), \
             'policies', (SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.id), '[]'::jsonb) \
                 FROM agent_policy_decisions p WHERE p.task_id = $1), \
             'evidence', (SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.id), '[]'::jsonb) \
                 FROM goal_evidence e WHERE e.task_id = $1), \
             'assertions', (SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.criterion_id), '[]'::jsonb) \
                 FROM goal_criterion_assertions a WHERE a.task_id = $1), \
             'blockers', (SELECT COALESCE(jsonb_agg(to_jsonb(b) ORDER BY b.id), '[]'::jsonb) \
                 FROM goal_blockers b WHERE b.task_id = $1), \
             'actions', (SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.id), '[]'::jsonb) \
                 FROM agent_task_actions a WHERE a.task_id = $1), \
             'events', (SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.sequence), '[]'::jsonb) \
                 FROM agent_task_events e WHERE e.task_id = $1))",
    )
    .bind(task_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

struct ScriptedModel {
    handler: Mutex<Arc<ModelHandler>>,
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    fn new() -> Self {
        Self {
            handler: Mutex::new(Arc::new(|_| {
                Box::pin(async {
                    Err(ModelError::Protocol(
                        "scripted model response is not configured".to_owned(),
                    ))
                })
            })),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn on_request<F, Fut>(&self, handler: F)
    where
        F: Fn(ModelRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ModelResponse, ModelError>> + Send + 'static,
    {
        *self.handler.lock().unwrap() = Arc::new(move |request| Box::pin(handler(request)));
    }

    fn complete(&self, outcome: &str, fields: Value) {
        let outcome = outcome.to_owned();
        self.on_request(move |_| {
            let outcome = outcome.clone();
            let fields = fields.clone();
            async move {
                Ok(ModelResponse {
                    items: vec![json!({
                        "type": "function_call",
                        "call_id": "done-1",
                        "name": "complete_task",
                        "arguments": {"outcome": outcome, "fields": fields},
                    })],
                })
            }
        });
    }

    fn respond_with(&self, item: Value) {
        self.respond_with_items(vec![item]);
    }

    fn respond_with_items(&self, items: Vec<Value>) {
        self.on_request(move |_| {
            let items = items.clone();
            async move { Ok(ModelResponse { items }) }
        });
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ModelClient for ScriptedModel {
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        let handler = self.handler.lock().unwrap().clone();
        handler(request).await
    }
}

struct BridgeFixture {
    dispatcher: AgentCommandDispatcher,
    kernel: Arc<AgentKernel>,
    store: Arc<PostgresStore>,
    model: Arc<ScriptedModel>,
    pool: PgPool,
    principal: PrincipalId,
    session_id: kiteframe::SessionId,
    task_id: UserTaskId,
}

impl BridgeFixture {
    async fn started() -> Option<Self> {
        Self::started_with_task(
            "tools []",
            r#"on ok(answer: string) { done answer }"#,
            false,
        )
        .await
    }

    async fn started_with_declared_tool() -> Option<Self> {
        Self::started_with_task(
            "tools [search]",
            r#"on ok(answer: string) { done answer }"#,
            true,
        )
        .await
    }

    async fn started_with_approval() -> Option<Self> {
        Self::started_with_task(
            "tools []",
            r#"on needs_approval() {
      answer = ask string "Approve?"
      done answer
    }"#,
            false,
        )
        .await
    }

    async fn started_with_blocking_outcome() -> Option<Self> {
        Self::started_with_task(
            "tools []",
            r#"on unavailable(reason: string) {
      block goal when 0 {
        code: "required_access_unavailable"
        explanation: reason
        evidence: [current_evidence]
        unlock: ["Restore the required access."]
      }
    }"#,
            false,
        )
        .await
    }

    async fn started_with_multiple_outcomes() -> Option<Self> {
        Self::started_with_task(
            "tools []",
            r#"on ok(answer: string) { done answer }
    on unavailable(reason: string) { done reason }"#,
            false,
        )
        .await
    }

    async fn started_with_task(tools: &str, outcomes: &str, declared_tool: bool) -> Option<Self> {
        let base_url = std::env::var("DATABASE_URL").ok()?;
        let database = IsolatedDatabase::fresh(&base_url).await;
        let database_url = database.url;
        let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let principal = PrincipalId::new(format!("agent-bridge-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/agent-bridge".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let catalog_tools = declared_tool
            .then(|| {
                CatalogTool::new(
                    "docs/search",
                    "docs",
                    "Search documents.",
                    ClosedSchema::object("input", Default::default()),
                    ClosedSchema::object("output", Default::default()),
                    BTreeSet::new(),
                    EffectKind::ReadOnly,
                    RecoverySupport::NotRequired,
                )
                .unwrap()
            })
            .into_iter()
            .collect::<Vec<_>>();
        let catalog =
            ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), catalog_tools).unwrap();
        store.append_catalog_epoch(&catalog).await.unwrap();
        let name = format!("bridge_agent_{}", Uuid::now_v7().simple());
        let source = format!(
            r#"agent {name}(repository: string, pull_request: integer) -> string {{
  entry inspect(repository = repository, pull_request = pull_request)
  limits {{ steps: 20 model_turns_per_task: 3 tool_calls_per_response: 1 parallel_branches: 1 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task inspect(repository: string, pull_request: integer) -> string {{
    model reasoning prompt """Inspect and finish one bounded task.""" {tools}
    {outcomes}
  }}
}}"#
        );
        let definition = compile(&source, &catalog, &Limits::default()).unwrap();
        let published = store
            .publish(session.id.clone(), &definition, &definition.source)
            .await
            .unwrap();
        let accepted = store
            .submit_user_task(
                &principal,
                SubmitUserTask {
                    session_id: session.id.clone(),
                    agent: AgentSelector {
                        name: published.name,
                        version: Some(published.version),
                    },
                    target: TaskTarget::GithubPullRequest {
                        repository: "acme/widgets".to_owned(),
                        number: 142,
                    },
                    preferences: TaskPreferences { harness_role: None },
                    goal: GoalContract {
                        version: 1,
                        objective: "Resolve review feedback and verify the change.".to_owned(),
                        success: vec![SuccessCriterion {
                            id: "reviews_resolved".to_owned(),
                            statement: "All actionable review threads are resolved.".to_owned(),
                            evidence: "harness.invocation_result".to_owned(),
                        }],
                        constraints: vec!["Never merge the pull request.".to_owned()],
                        blocked_when: vec![
                            "Required user or reviewer action remains unavailable.".to_owned(),
                        ],
                    },
                    idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let kernel = Arc::new(AgentKernel::new(
            Arc::clone(&store),
            Arc::new(kiteframe::KiteAgentRuntime::new(RuntimeCeilings::default())),
            Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&store))),
        ));
        kernel
            .start_agent(
                &principal,
                StartAgent {
                    session_id: session.id.clone(),
                    task_id: accepted.id.clone(),
                    expected_task_revision: 0,
                    expected_goal_revision: 0,
                    idempotency_key: IdempotencyKey::new(format!("start-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let model = Arc::new(ScriptedModel::new());
        let profile = builtin_profile();
        let plugin = Arc::new(BuiltinHarnessPlugin::new(profile.clone(), model.clone()));
        let mut registry = HarnessRegistry::new();
        registry.register(plugin).await.unwrap();
        store
            .register_harness_plugin(HarnessPluginRegistration {
                profile_id: profile.id.clone(),
                profile_digest: profile.digest().unwrap(),
                source_repository: profile.source_repository.clone(),
                source_revision: profile.source_revision.clone(),
                transport: profile.transport,
                enabled: true,
            })
            .await
            .unwrap();
        let dispatcher = kernel
            .clone()
            .with_harness_registry(Arc::new(registry), Arc::new(DenyAllHarnessApprovals));
        Some(Self {
            dispatcher,
            kernel,
            pool: store.pool().clone(),
            store,
            model,
            principal,
            session_id: session.id,
            task_id: accepted.id,
        })
    }

    async fn pending_command_id(&self) -> Uuid {
        *self.pending_command().await.id.as_uuid()
    }

    async fn pending_command(&self) -> AgentCommandView {
        self.store
            .dispatchable_agent_commands(256)
            .await
            .unwrap()
            .into_iter()
            .find(|command| command.task_id == self.task_id)
            .unwrap()
    }

    async fn current(&self) -> AgentExecutionView {
        self.store
            .get_agent_execution(self.session_id.clone(), self.task_id.clone())
            .await
            .unwrap()
    }

    async fn pending_invocation(&self) -> InvokeHarness {
        let command_id = self.pending_command_id().await;
        let context = self
            .store
            .load_agent_command_context(
                self.session_id.clone(),
                self.task_id.clone(),
                AgentCommandId::new(command_id),
            )
            .await
            .unwrap();
        let KernelCommandPayload::InvokeHarness(invocation) = context.command.payload else {
            panic!("pending command must invoke the harness")
        };
        invocation
    }

    async fn evidence(&self) -> Vec<EvidenceRecord> {
        sqlx::query(
            "SELECT id, evidence_type, source_identity, content_digest \
             FROM goal_evidence WHERE task_id = $1 ORDER BY id",
        )
        .bind(self.task_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| EvidenceRecord {
            id: EvidenceId::new(row.get("id")),
            task_id: self.task_id.clone(),
            evidence_type: row.get("evidence_type"),
            source: EvidenceSourceIdentity::BuiltinHarness {
                command_id: AgentCommandId::new(row.get("source_identity")),
            },
            digest: row.get("content_digest"),
        })
        .collect()
    }

    async fn legacy_run_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE session_id = $1")
            .bind(self.session_id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn legacy_frame_count(&self) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM frames f JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
        )
        .bind(self.session_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn command_state(&self, command_id: &AgentCommandId) -> String {
        sqlx::query_scalar("SELECT state FROM agent_commands WHERE id = $1")
            .bind(command_id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn seed_legacy_run_with_history(&self) -> kiteframe::RunId {
        let catalog = self
            .store
            .current_catalog(self.session_id.clone())
            .await
            .unwrap();
        let name = format!("legacy_bridge_{}", Uuid::now_v7().simple());
        let source = format!(
            "workflow {name}() -> string {{ start inspect() }} task inspect() -> string {{ prompt \"\"\"legacy history\"\"\" tools [] on ok(answer: string) {{ done answer }} }}"
        );
        let definition = compile(&source, &catalog, &Limits::default()).unwrap();
        let published = self
            .store
            .publish(self.session_id.clone(), &definition, &source)
            .await
            .unwrap();
        let run = self
            .store
            .start_run(StartRun {
                session_id: self.session_id.clone(),
                workflow_name: published.name,
                version: Some(published.version),
                input: json!({}),
                idempotency_key: IdempotencyKey::new(format!("legacy-{}", Uuid::now_v7())).unwrap(),
            })
            .await
            .unwrap();
        let attempt = self
            .store
            .begin_model_attempt(
                run.root_frame_id.clone(),
                0,
                json!({"prompt": "immutable legacy prompt"}),
            )
            .await
            .unwrap();
        self.store
            .commit_model_attempt(
                run.root_frame_id,
                attempt.attempt,
                attempt.revision,
                vec![json!({"type": "message", "text": "immutable legacy result"})],
            )
            .await
            .unwrap();
        run.id
    }

    async fn legacy_fingerprint(&self, run_id: &kiteframe::RunId) -> Value {
        sqlx::query_scalar(
            "SELECT jsonb_build_object( \
                 'run', (SELECT to_jsonb(r) FROM runs r WHERE r.id = $1), \
                 'frames', (SELECT COALESCE(jsonb_agg(to_jsonb(f) ORDER BY f.id), '[]'::jsonb) \
                     FROM frames f WHERE f.run_id = $1), \
                 'history', (SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.frame_id, m.attempt, m.position, m.direction), '[]'::jsonb) \
                     FROM model_items m JOIN frames f ON f.id = m.frame_id WHERE f.run_id = $1), \
                 'events', (SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.sequence), '[]'::jsonb) \
                     FROM events e WHERE e.run_id = $1), \
                 'toolBindings', (SELECT COALESCE(jsonb_agg(to_jsonb(b) ORDER BY b.qualified_name), '[]'::jsonb) \
                     FROM run_tool_bindings b WHERE b.run_id = $1))",
        )
        .bind(run_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn event_kinds(&self) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT kind FROM agent_task_events WHERE task_id = $1 ORDER BY sequence",
        )
        .bind(self.task_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn snapshot_fingerprint(&self) -> (Value, String, i32, i32) {
        let row = sqlx::query(
            "SELECT payload, snapshot_digest, canonical_bytes, ceiling_bytes \
             FROM agent_runtime_snapshots WHERE task_id = $1",
        )
        .bind(self.task_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (
            row.get("payload"),
            row.get("snapshot_digest"),
            row.get("canonical_bytes"),
            row.get("ceiling_bytes"),
        )
    }

    async fn cleanup(&self) {
        let current = self.current().await;
        if matches!(
            current.lifecycle,
            TaskLifecycle::Accepted | TaskLifecycle::Running
        ) && current.goal_phase != GoalPhase::Complete
        {
            self.kernel
                .cancel_task(
                    &self.principal,
                    CancelUserTask {
                        session_id: self.session_id.clone(),
                        task_id: self.task_id.clone(),
                        expected_task_revision: current.task_revision,
                        expected_goal_revision: current.goal_revision,
                        reason: Some("Bridge test cleanup.".to_owned()),
                        idempotency_key: IdempotencyKey::new(format!("cleanup-{}", Uuid::now_v7()))
                            .unwrap(),
                    },
                )
                .await
                .unwrap();
        }
    }
}

async fn builtin_dispatcher_with_fault(
    fixture: &BridgeFixture,
    fault: Arc<HarnessDispatcherTestFault>,
    max_pre_accept_attempts: u64,
) -> AgentCommandDispatcher {
    let profile = builtin_profile();
    let plugin = Arc::new(BuiltinHarnessPlugin::new(profile, fixture.model.clone()));
    let mut registry = HarnessRegistry::new();
    registry
        .register_with_pre_accept_attempt_limit(plugin, max_pre_accept_attempts)
        .await
        .unwrap();
    fixture
        .kernel
        .clone()
        .with_harness_registry(Arc::new(registry), Arc::new(DenyAllHarnessApprovals))
        .with_test_fault(fault)
}

#[derive(Clone, Copy)]
enum SelectedRuntimeProfile {
    Absent,
    Matching,
    Mismatched,
}

async fn selected_profile_dispatcher(
    fixture: &BridgeFixture,
    runtime: SelectedRuntimeProfile,
    barrier: Option<Arc<HarnessDispatcherTestBarrier>>,
) -> (HarnessProfile, AgentCommandDispatcher) {
    let selected = HarnessProfile::from_json(include_bytes!(
        "../config/harness-profiles/codex-acp-v1.json"
    ))
    .unwrap();
    fixture
        .store
        .register_harness_plugin(HarnessPluginRegistration {
            profile_id: selected.id.clone(),
            profile_digest: selected.digest().unwrap(),
            source_repository: selected.source_repository.clone(),
            source_revision: selected.source_revision.clone(),
            transport: selected.transport,
            enabled: true,
        })
        .await
        .unwrap();
    let builtin = builtin_profile();
    let mut registry = HarnessRegistry::new();
    registry
        .register(Arc::new(BuiltinHarnessPlugin::new(
            builtin.clone(),
            fixture.model.clone(),
        )))
        .await
        .unwrap();
    if !matches!(runtime, SelectedRuntimeProfile::Absent) {
        let mut runtime_profile = selected.clone();
        if matches!(runtime, SelectedRuntimeProfile::Mismatched) {
            runtime_profile.source_repository.push_str("/digest-drift");
        }
        registry
            .register(Arc::new(BuiltinHarnessPlugin::new(
                runtime_profile,
                fixture.model.clone(),
            )))
            .await
            .unwrap();
    }
    let routes = HarnessConfig::from_test_values([(
        "KITEFRAME_REASONING_HARNESS_PROFILE",
        selected.id.as_str(),
    )])
    .unwrap()
    .harness_routes(builtin.id);
    let dispatcher = fixture.kernel.clone().with_harness_registry_routes(
        Arc::new(registry),
        Arc::new(DenyAllHarnessApprovals),
        routes,
    );
    let dispatcher = match barrier {
        Some(barrier) => dispatcher.with_test_barrier(barrier),
        None => dispatcher,
    };
    (selected, dispatcher)
}

#[tokio::test]
async fn invocation_is_persisted_and_claimed_before_model_call() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let command_id = fixture.pending_command_id().await;
    fixture.model.on_request({
        let pool = fixture.pool.clone();
        move |_| {
            let pool = pool.clone();
            async move {
                let state: String =
                    sqlx::query_scalar("SELECT state FROM agent_commands WHERE id = $1")
                        .bind(command_id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                assert_eq!(state, "claimed");
                let invocation: (String, String, String, Option<String>) = sqlx::query_as(
                    "SELECT profile_id, state, payload_digest, terminal_request_digest \
                     FROM harness_invocations WHERE command_id = $1",
                )
                .bind(command_id)
                .fetch_one(&pool)
                .await
                .unwrap();
                assert_eq!(invocation.0, builtin_profile().id.as_str());
                assert_eq!(invocation.1, "accepted");
                assert!(!invocation.2.is_empty());
                assert!(invocation.3.is_none());
                Ok(ModelResponse {
                    items: vec![json!({
                        "type": "function_call",
                        "call_id": "done-1",
                        "name": "complete_task",
                        "arguments": {"outcome": "ok", "fields": {"answer": "done"}},
                    })],
                })
            }
        }
    });

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    assert_eq!(fixture.current().await.lifecycle, TaskLifecycle::Succeeded);
    let invocation: (Uuid, String, String) = sqlx::query_as(
        "SELECT invocation_id, state, terminal_request_digest \
         FROM harness_invocations WHERE command_id = $1",
    )
    .bind(command_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(invocation.1, "completed");
    assert_eq!(invocation.2, format!("harness-terminal:{}", invocation.0));
    let source_event_id: String = sqlx::query_scalar(
        "SELECT source_event_id FROM agent_transitions WHERE task_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(fixture.task_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(source_event_id, format!("harness-result:{}", invocation.0));
}

#[tokio::test]
async fn selected_unconfigured_profile_settles_one_durable_failure_without_fallback_or_replay() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(mut fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    let goal_before: Value =
        sqlx::query_scalar("SELECT to_jsonb(goal_states) FROM goal_states WHERE task_id = $1")
            .bind(fixture.task_id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let (selected, dispatcher) =
        selected_profile_dispatcher(&fixture, SelectedRuntimeProfile::Absent, None).await;
    let selected_digest = selected.digest().unwrap();
    fixture.dispatcher = dispatcher;

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let claimed_revision = command.revision + 1;
    let invocation = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, claimed_revision)
        .await
        .unwrap()
        .unwrap();
    let terminal_key = format!("harness-terminal:{}", invocation.invocation_id.as_uuid());
    let terminal_payload = json!({
        "type": "failed",
        "code": "harness_profile_unavailable",
        "detail": "selected harness profile is not registered",
    });
    let command_result = json!({
        "type": "command_failed",
        "payload": {
            "event_id": format!("command-failed:{}:{}", command.id.as_uuid(), terminal_key),
            "command_key": command.key,
            "code": "harness_profile_unavailable",
        }
    });
    let expected_request_digest = canonical_sha256(&json!({
        "sessionId": fixture.session_id.as_uuid(),
        "taskId": fixture.task_id.as_uuid(),
        "commandId": command.id.as_uuid(),
        "expectedCommandRevision": claimed_revision,
        "event": command_result,
    }))
    .unwrap();
    let settled_command: (
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<Value>,
        Option<Value>,
    ) = sqlx::query_as(
        "SELECT state, revision, completion_idempotency_key, completion_request_digest, \
                    result, failure FROM agent_commands WHERE id = $1",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let terminal_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM harness_invocations WHERE command_id = $1")
            .bind(command.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let harness_event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM harness_invocation_events WHERE invocation_id = $1",
    )
    .bind(invocation.invocation_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let goal_after: Value =
        sqlx::query_scalar("SELECT to_jsonb(goal_states) FROM goal_states WHERE task_id = $1")
            .bind(fixture.task_id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();

    assert_eq!(invocation.profile_id, selected.id);
    assert_eq!(invocation.profile_digest, selected_digest);
    assert_eq!(invocation.state, HarnessInvocationState::Failed);
    assert!(invocation.terminal);
    assert_eq!(
        invocation.terminal_request_digest,
        Some(terminal_key.clone())
    );
    assert_eq!(invocation.terminal_payload, Some(terminal_payload));
    assert_eq!(settled_command.0, "completed");
    assert_eq!(
        settled_command.1,
        i64::try_from(claimed_revision + 1).unwrap()
    );
    assert_eq!(settled_command.2, Some(terminal_key));
    assert_eq!(settled_command.3, Some(expected_request_digest));
    assert_eq!(settled_command.4, Some(command_result));
    assert_eq!(settled_command.5, None);
    assert_eq!(terminal_count, 1);
    assert_eq!(harness_event_count, 0);
    assert_eq!(fixture.model.calls(), 0);
    assert!(fixture.evidence().await.is_empty());
    assert_eq!(goal_after, goal_before);
    assert_eq!(fixture.current().await.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(
        fixture
            .dispatcher
            .recover_harness_invocations(8)
            .await
            .unwrap(),
        kiteframe::HarnessRecoveryReport::default()
    );
}

#[tokio::test]
async fn cancellation_wins_over_selected_unconfigured_profile_failure() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    let barrier = Arc::new(HarnessDispatcherTestBarrier::new(
        HarnessDispatcherTestBarrierPoint::BeforeCommandTerminal,
    ));
    let (_, dispatcher) = selected_profile_dispatcher(
        &fixture,
        SelectedRuntimeProfile::Absent,
        Some(barrier.clone()),
    )
    .await;
    let dispatch = tokio::spawn(async move { dispatcher.tick_agent_commands(8).await });
    if tokio::time::timeout(
        std::time::Duration::from_secs(5),
        barrier.wait_until_entered(),
    )
    .await
    .is_err()
    {
        panic!(
            "unconfigured profile never reached durable terminal authority: {:?}",
            dispatch.await.unwrap()
        );
    }
    let current = fixture.current().await;
    fixture
        .kernel
        .cancel_task(
            &fixture.principal,
            CancelUserTask {
                session_id: fixture.session_id.clone(),
                task_id: fixture.task_id.clone(),
                expected_task_revision: current.task_revision,
                expected_goal_revision: current.goal_revision,
                reason: Some("Principal cancelled unavailable profile dispatch.".to_owned()),
                idempotency_key: IdempotencyKey::new(format!(
                    "cancel-profile-unavailable-{}",
                    Uuid::now_v7()
                ))
                .unwrap(),
            },
        )
        .await
        .unwrap();
    barrier.release();
    assert_eq!(dispatch.await.unwrap().unwrap(), 1);

    let invocation = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, command.revision + 1)
        .await
        .unwrap()
        .unwrap();
    let execution = fixture.current().await;
    assert_eq!(execution.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(execution.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.command_state(&command.id).await, "cancelled");
    assert_eq!(invocation.state, HarnessInvocationState::Cancelled);
    assert!(invocation.terminal);
    assert_eq!(
        invocation.terminal_request_digest,
        Some(format!(
            "harness-terminal:{}",
            invocation.invocation_id.as_uuid()
        ))
    );
    assert_eq!(
        invocation.terminal_payload,
        Some(json!({"type":"cancelled"}))
    );
    assert_eq!(fixture.model.calls(), 0);
    assert!(fixture.evidence().await.is_empty());
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(
        fixture
            .dispatcher
            .recover_harness_invocations(8)
            .await
            .unwrap(),
        kiteframe::HarnessRecoveryReport::default()
    );
}

#[tokio::test]
async fn restart_reuses_unconfigured_profile_invocation_and_settles_it_once() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    let barrier = Arc::new(HarnessDispatcherTestBarrier::new(
        HarnessDispatcherTestBarrierPoint::AfterPrepare,
    ));
    let (_, first) = selected_profile_dispatcher(
        &fixture,
        SelectedRuntimeProfile::Absent,
        Some(barrier.clone()),
    )
    .await;
    let interrupted = tokio::spawn(async move { first.tick_agent_commands(8).await });
    barrier.wait_until_entered().await;
    let prepared = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, command.revision + 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prepared.state, HarnessInvocationState::Prepared);
    assert!(!prepared.terminal);
    interrupted.abort();
    barrier.release();
    assert!(interrupted.await.unwrap_err().is_cancelled());

    let (_, restarted) =
        selected_profile_dispatcher(&fixture, SelectedRuntimeProfile::Absent, None).await;
    assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
    let settled = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, command.revision + 1)
        .await
        .unwrap()
        .unwrap();
    let invocation_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM harness_invocations WHERE command_id = $1")
            .bind(command.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();

    assert_eq!(settled.invocation_id, prepared.invocation_id);
    assert_eq!(settled.state, HarnessInvocationState::Failed);
    assert!(settled.terminal);
    assert_eq!(
        settled.terminal_payload,
        Some(json!({
            "type":"failed",
            "code":"harness_profile_unavailable",
            "detail":"selected harness profile is not registered",
        }))
    );
    assert_eq!(invocation_count, 1);
    assert_eq!(fixture.model.calls(), 0);
    assert!(fixture.evidence().await.is_empty());
    assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(
        restarted.recover_harness_invocations(8).await.unwrap(),
        kiteframe::HarnessRecoveryReport::default()
    );
}

#[tokio::test]
async fn mismatched_runtime_profile_digest_fails_before_plugin_work_and_attests_durable_identity() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(mut fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    let goal_before: Value =
        sqlx::query_scalar("SELECT to_jsonb(goal_states) FROM goal_states WHERE task_id = $1")
            .bind(fixture.task_id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    fixture
        .model
        .complete("ok", json!({"answer":"must not execute"}));
    let (durable, dispatcher) =
        selected_profile_dispatcher(&fixture, SelectedRuntimeProfile::Mismatched, None).await;
    let durable_digest = durable.digest().unwrap();
    fixture.dispatcher = dispatcher;

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let invocation = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, command.revision + 1)
        .await
        .unwrap()
        .unwrap();
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM harness_invocation_events WHERE invocation_id = $1",
    )
    .bind(invocation.invocation_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let command_result: Value =
        sqlx::query_scalar("SELECT result FROM agent_commands WHERE id = $1")
            .bind(command.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let goal_after: Value =
        sqlx::query_scalar("SELECT to_jsonb(goal_states) FROM goal_states WHERE task_id = $1")
            .bind(fixture.task_id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();

    assert_eq!(invocation.profile_id, durable.id);
    assert_eq!(invocation.profile_digest, durable_digest);
    assert_eq!(invocation.state, HarnessInvocationState::Failed);
    assert!(invocation.terminal);
    assert_eq!(
        invocation.terminal_payload,
        Some(json!({
            "type":"failed",
            "code":"harness_protocol_incompatible",
            "detail":"runtime harness profile digest does not match durable registration",
        }))
    );
    assert_eq!(fixture.model.calls(), 0);
    assert_eq!(event_count, 0);
    assert!(fixture.evidence().await.is_empty());
    assert_eq!(
        command_result["payload"]["code"],
        "harness_protocol_incompatible"
    );
    assert_eq!(goal_after, goal_before);
    assert_eq!(fixture.current().await.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(
        fixture
            .dispatcher
            .recover_harness_invocations(8)
            .await
            .unwrap(),
        kiteframe::HarnessRecoveryReport::default()
    );
}

#[tokio::test]
async fn matching_runtime_profile_digest_executes_under_the_same_durable_identity() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(mut fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    fixture
        .model
        .complete("ok", json!({"answer":"exact identity"}));
    let (durable, dispatcher) =
        selected_profile_dispatcher(&fixture, SelectedRuntimeProfile::Matching, None).await;
    let durable_digest = durable.digest().unwrap();
    fixture.dispatcher = dispatcher;

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let invocation = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, command.revision + 1)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(invocation.profile_id, durable.id);
    assert_eq!(invocation.profile_digest, durable_digest);
    assert_eq!(invocation.state, HarnessInvocationState::Completed);
    assert!(invocation.terminal);
    assert_eq!(fixture.model.calls(), 1);
    assert_eq!(fixture.evidence().await.len(), 1);
    assert_eq!(fixture.current().await.goal_phase, GoalPhase::Active);
}

#[tokio::test]
async fn terminal_journal_replay_keeps_the_old_profile_after_a_builtin_upgrade() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    let claimed = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session_id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: command.id.clone(),
            expected_command_revision: command.revision,
            claimant: "harness-dispatcher-v1".to_owned(),
            idempotency_key: IdempotencyKey::new(format!(
                "harness-dispatcher-v1:{}",
                command.id.as_uuid()
            ))
            .unwrap(),
        })
        .await
        .unwrap();
    let profile = builtin_harness_profile("1123456789abcdef0123456789abcdef01234567").unwrap();
    let profile_digest = profile.digest().unwrap();
    fixture
        .store
        .register_harness_plugin(HarnessPluginRegistration {
            profile_id: profile.id.clone(),
            profile_digest: profile_digest.clone(),
            source_repository: profile.source_repository.clone(),
            source_revision: profile.source_revision.clone(),
            transport: profile.transport,
            enabled: true,
        })
        .await
        .unwrap();
    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    let invocation = fixture
        .store
        .prepare_harness_invocation(PrepareHarnessInvocation {
            invocation_id: &invocation_id,
            command_id: &claimed.id,
            command_revision: claimed.revision,
            profile_id: &profile.id,
            profile_digest: &profile_digest,
            payload_digest: &claimed.payload_digest,
        })
        .await
        .unwrap();
    let started = fixture
        .store
        .mark_harness_invocation_started(&invocation_id, invocation.attempt)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation_id, started.attempt)
        .await
        .unwrap();
    let terminal_key = format!("harness-terminal:{}", invocation_id.as_uuid());
    force_legacy_terminal_journal(
        &fixture.pool,
        &invocation_id,
        "completed",
        &terminal_key,
        json!({
            "type":"completed",
            "outcome":"ok",
            "fields":{"answer":"replayed"},
            "history":[],
        }),
    )
    .await;

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    assert_eq!(
        fixture.model.calls(),
        0,
        "terminal replay never calls the model"
    );
    assert_eq!(fixture.current().await.lifecycle, TaskLifecycle::Succeeded);
    assert_eq!(fixture.current().await.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.command_state(&command.id).await, "completed");
    assert_eq!(fixture.evidence().await.len(), 1);
    let replayed_profile_id: String =
        sqlx::query_scalar("SELECT profile_id FROM harness_invocations WHERE invocation_id = $1")
            .bind(invocation_id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(replayed_profile_id, profile.id.as_str());
    assert_ne!(replayed_profile_id, builtin_profile().id.as_str());
    let terminal_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM harness_invocations WHERE command_id = $1 AND terminal = TRUE",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(terminal_count, 1);
    let source_event_id: String = sqlx::query_scalar(
        "SELECT source_event_id FROM agent_transitions WHERE task_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(fixture.task_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        source_event_id,
        format!("harness-result:{}", invocation_id.as_uuid())
    );

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(fixture.evidence().await.len(), 1);
}

#[tokio::test]
async fn failed_terminal_journal_replay_converges_the_kernel_failure_once() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    let claimed = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session_id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: command.id.clone(),
            expected_command_revision: command.revision,
            claimant: "harness-dispatcher-v1".to_owned(),
            idempotency_key: IdempotencyKey::new(format!(
                "harness-dispatcher-v1:{}",
                command.id.as_uuid()
            ))
            .unwrap(),
        })
        .await
        .unwrap();
    let profile = builtin_profile();
    let profile_digest = profile.digest().unwrap();
    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    let invocation = fixture
        .store
        .prepare_harness_invocation(PrepareHarnessInvocation {
            invocation_id: &invocation_id,
            command_id: &claimed.id,
            command_revision: claimed.revision,
            profile_id: &profile.id,
            profile_digest: &profile_digest,
            payload_digest: &claimed.payload_digest,
        })
        .await
        .unwrap();
    let started = fixture
        .store
        .mark_harness_invocation_started(&invocation_id, invocation.attempt)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation_id, started.attempt)
        .await
        .unwrap();
    let terminal_key = format!("harness-terminal:{}", invocation_id.as_uuid());
    force_legacy_terminal_journal(
        &fixture.pool,
        &invocation_id,
        "failed",
        &terminal_key,
        json!({
            "type":"failed",
            "code":"harness_failed",
            "detail":"replayed failure",
        }),
    )
    .await;

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    assert_eq!(fixture.model.calls(), 0);
    let current = fixture.current().await;
    assert_eq!(current.lifecycle, TaskLifecycle::Failed);
    assert_eq!(current.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.command_state(&command.id).await, "completed");
    assert!(fixture.evidence().await.is_empty());
    let failure_key: Option<String> =
        sqlx::query_scalar("SELECT completion_idempotency_key FROM agent_commands WHERE id = $1")
            .bind(command.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(failure_key.as_deref(), Some(terminal_key.as_str()));

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert!(fixture.evidence().await.is_empty());
}

#[tokio::test]
async fn non_positive_tick_limits_do_not_list_or_dispatch_work() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    fixture
        .model
        .complete("ok", json!({"answer": "must not run"}));

    assert_eq!(fixture.dispatcher.tick_agent_commands(0).await.unwrap(), 0);
    assert_eq!(fixture.dispatcher.tick_agent_commands(-1).await.unwrap(), 0);
    assert_eq!(fixture.model.calls(), 0);
    assert_eq!(fixture.command_state(&command.id).await, "pending");
    fixture.cleanup().await;
}

#[tokio::test]
async fn harness_success_mints_evidence_but_does_not_complete_goal() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let legacy_run = fixture.seed_legacy_run_with_history().await;
    let legacy_before = fixture.legacy_fingerprint(&legacy_run).await;
    fixture.model.complete("ok", json!({"answer": "done"}));

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Succeeded);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    let evidence = fixture.evidence().await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].evidence_type, "harness.invocation_result");
    assert!(matches!(
        evidence[0].source,
        EvidenceSourceIdentity::BuiltinHarness { .. }
    ));
    assert_eq!(fixture.legacy_run_count().await, 1);
    assert_eq!(fixture.legacy_frame_count().await, 1);
    assert_eq!(fixture.legacy_fingerprint(&legacy_run).await, legacy_before);
}

#[tokio::test]
async fn actual_model_request_preserves_the_full_durable_goal_and_authority_boundary() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let invocation = fixture.pending_invocation().await;
    fixture.model.complete("ok", json!({"answer": "done"}));
    let executor = BuiltinHarnessPlugin::new(builtin_profile(), fixture.model.clone());

    executor.execute_direct(&invocation).await.unwrap();
    executor.execute_direct(&invocation).await.unwrap();

    let requests = fixture.model.requests();
    assert_eq!(requests.len(), 2);
    let expected_goal = json!({
        "objective": "Resolve review feedback and verify the change.",
        "success": [{
            "id": "reviews_resolved",
            "statement": "All actionable review threads are resolved.",
            "evidenceType": "harness.invocation_result",
        }],
        "constraints": ["Never merge the pull request."],
        "blockedWhen": ["Required user or reviewer action remains unavailable."],
    });
    assert_eq!(requests[0].input[0]["goal"], expected_goal);
    assert_eq!(requests[1].input[0]["goal"], expected_goal);
    assert_eq!(requests[0].instructions, requests[1].instructions);
    for required in [
        "durable across this invocation and every repeated or recovered invocation",
        "complete_task function completes only this bounded harness task",
        "yields invocation evidence",
        "never completes or blocks the enclosing Kiteframe goal",
        "Rust kernel's CompleteGoal and BlockGoal authority",
    ] {
        assert!(
            requests[0].instructions.contains(required),
            "missing durable-goal directive: {required}"
        );
    }
    assert!(
        requests[0]
            .instructions
            .starts_with("Inspect and finish one bounded task.")
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn completion_tool_matches_the_nested_complete_task_arguments_contract() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let invocation = fixture.pending_invocation().await;
    fixture.model.complete("ok", json!({"answer": "done"}));
    BuiltinHarnessPlugin::new(builtin_profile(), fixture.model.clone())
        .execute_direct(&invocation)
        .await
        .unwrap();

    let requests = fixture.model.requests();
    let completion = &requests[0].tools[0];
    assert_eq!(completion["type"], "function");
    assert_eq!(completion["name"], "complete_task");
    assert_eq!(
        completion["parameters"]["required"],
        json!(["outcome", "fields"])
    );
    assert_eq!(
        completion["parameters"]["properties"]["outcome"]["enum"],
        json!(["ok"])
    );
    assert_eq!(
        completion["parameters"]["properties"]["fields"]["required"],
        json!(["answer"])
    );
    assert!(completion["parameters"]["properties"]["fields"]["properties"]["answer"].is_object());
    assert!(completion["parameters"]["properties"]["answer"].is_null());
    fixture.cleanup().await;
}

#[tokio::test]
async fn declared_model_tool_fails_closed_without_legacy_or_goal_authority() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started_with_declared_tool().await else {
        return;
    };
    let command = fixture.pending_command().await;
    fixture.model.respond_with(json!({
        "type": "function_call",
        "call_id": "tool-1",
        "name": "docs/search",
        "arguments": {},
    }));

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Failed);
    assert_eq!(view.operational_status, OperationalStatus::Finished);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.command_state(&command.id).await, "failed");
    let requests = fixture.model.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].tools.len(), 2);
    assert_eq!(requests[0].tools[0]["name"], "complete_task");
    assert_eq!(requests[0].tools[1]["name"], "docs/search");
    assert_eq!(
        requests[0].tools[1]["parameters"],
        json!({
            "type":"object",
            "name":"input",
            "required":[],
            "properties":{},
            "additionalProperties":false
        })
    );
    let failure: Value = sqlx::query_scalar("SELECT failure FROM user_tasks WHERE id = $1")
        .bind(fixture.task_id.as_uuid())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(failure["code"], "runtime_command_unavailable");
    assert!(
        fixture
            .event_kinds()
            .await
            .contains(&"runtime.command_unavailable".to_owned())
    );
    assert_eq!(fixture.legacy_run_count().await, 0);
    assert_eq!(fixture.legacy_frame_count().await, 0);
}

#[tokio::test]
async fn restart_after_builtin_command_failure_reconstructs_the_exact_terminal_once() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(uninterrupted) = BridgeFixture::started_with_declared_tool().await else {
        eprintln!("SKIP built-in failed-command recovery: DATABASE_URL is absent");
        return;
    };
    let uninterrupted_command = uninterrupted.pending_command().await;
    uninterrupted.model.respond_with(json!({
        "type": "function_call",
        "call_id": "tool-uninterrupted",
        "name": "docs/search",
        "arguments": {},
    }));
    assert_eq!(
        uninterrupted
            .dispatcher
            .tick_agent_commands(8)
            .await
            .unwrap(),
        1
    );
    let uninterrupted_payload: Value = sqlx::query_scalar(
        "SELECT terminal_payload FROM harness_invocations WHERE command_id = $1",
    )
    .bind(uninterrupted_command.id.as_uuid())
    .fetch_one(&uninterrupted.pool)
    .await
    .unwrap();
    uninterrupted.cleanup().await;
    drop(uninterrupted);

    let Some(fixture) = BridgeFixture::started_with_declared_tool().await else {
        eprintln!("SKIP built-in failed-command recovery: DATABASE_URL is absent");
        return;
    };
    let command = fixture.pending_command().await;
    fixture.model.respond_with(json!({
        "type": "function_call",
        "call_id": "tool-1",
        "name": "docs/search",
        "arguments": {},
    }));
    let barrier = Arc::new(HarnessDispatcherTestBarrier::new(
        HarnessDispatcherTestBarrierPoint::AfterCommandTerminal,
    ));
    let dispatcher = Arc::new(fixture.dispatcher.with_test_barrier(barrier.clone()));
    let first = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.tick_agent_commands(8).await }
    });
    barrier.wait_until_entered().await;
    let before_restart: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('state', state, 'key', completion_idempotency_key, \
         'digest', completion_request_digest, 'result', result, 'failure', failure) \
         FROM agent_commands WHERE id = $1",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(before_restart["state"], "failed");
    assert_eq!(
        before_restart["failure"]["code"],
        "runtime_command_unavailable"
    );
    let invocation = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, 1)
        .await
        .unwrap()
        .unwrap();
    assert!(!invocation.terminal);
    assert!(matches!(
        fixture
            .store
            .settle_harness_invocation(
                &invocation.invocation_id,
                invocation.attempt,
                HarnessInvocationState::Failed,
                &format!("harness-terminal:{}", invocation.invocation_id.as_uuid()),
                &json!({
                    "type":"failed",
                    "code":"harness_capability_unsupported",
                    "detail":"arbitrary plugin detail must not enter the built-in schema",
                }),
            )
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    first.abort();
    barrier.release();
    assert!(first.await.unwrap_err().is_cancelled());

    let profile = builtin_profile();
    let mut registry = HarnessRegistry::new();
    registry
        .register(Arc::new(BuiltinHarnessPlugin::new(
            profile,
            fixture.model.clone(),
        )))
        .await
        .unwrap();
    let restarted = fixture
        .kernel
        .clone()
        .with_harness_registry(Arc::new(registry), Arc::new(DenyAllHarnessApprovals));
    let report = restarted.recover_harness_invocations(1).await.unwrap();
    assert_eq!(report.terminal, 1);
    assert_eq!(report.retryable_pre_accept, 0);
    assert_eq!(report.outcome_unknown, 0);
    let recovered = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.state, HarnessInvocationState::Failed);
    assert_eq!(
        recovered.terminal_payload,
        Some(uninterrupted_payload.clone())
    );
    assert_eq!(
        uninterrupted_payload,
        json!({
            "type":"failed",
            "code":"harness_capability_unsupported",
            "detail":"built-in harness capability is unavailable",
        })
    );
    let after_restart: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('state', state, 'key', completion_idempotency_key, \
         'digest', completion_request_digest, 'result', result, 'failure', failure) \
         FROM agent_commands WHERE id = $1",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(after_restart, before_restart);
    assert_eq!(
        restarted.recover_harness_invocations(1).await.unwrap(),
        kiteframe::HarnessRecoveryReport::default()
    );
}

#[tokio::test]
async fn bounded_executor_rejects_missing_multiple_invalid_and_oversized_actions() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let invocation = fixture.pending_invocation().await;
    let executor = BuiltinHarnessPlugin::new(builtin_profile(), fixture.model.clone());

    fixture
        .model
        .respond_with(json!({"type": "message", "text": "done"}));
    assert!(matches!(
        executor.execute_direct(&invocation).await,
        Err(HarnessExecutionError::InvalidResult(message))
            if message.contains("exactly one")
    ));

    fixture.model.respond_with_items(vec![
        json!({"type":"function_call", "call_id":"done-1", "name":"complete_task", "arguments":{"outcome":"ok", "fields":{"answer":"one"}}}),
        json!({"type":"function_call", "call_id":"done-2", "name":"complete_task", "arguments":{"outcome":"ok", "fields":{"answer":"two"}}}),
    ]);
    assert!(matches!(
        executor.execute_direct(&invocation).await,
        Err(HarnessExecutionError::InvalidResult(message))
            if message.contains("exactly one")
    ));

    fixture.model.respond_with_items(vec![
        json!({"type":"function_call", "call_id":"done-3", "name":"complete_task", "arguments":{}}),
        json!({"type":"function_call", "call_id":"tool-1", "name":"docs/search", "arguments":{}}),
    ]);
    assert!(matches!(
        executor.execute_direct(&invocation).await,
        Err(HarnessExecutionError::InvalidResult(message))
            if message.contains("exactly one")
    ));

    fixture.model.complete("unknown", json!({"answer": "done"}));
    assert!(matches!(
        executor.execute_direct(&invocation).await,
        Err(HarnessExecutionError::InvalidResult(_))
    ));

    fixture.model.complete("ok", json!({"answer": 7}));
    assert!(matches!(
        executor.execute_direct(&invocation).await,
        Err(HarnessExecutionError::InvalidResult(_))
    ));

    fixture.model.complete("ok", json!({}));
    assert!(matches!(
        executor.execute_direct(&invocation).await,
        Err(HarnessExecutionError::InvalidResult(message))
            if message.contains("do not match")
    ));

    let mut tiny = invocation.clone();
    tiny.limits.response_item_bytes = 1;
    fixture.model.complete("ok", json!({"answer": "done"}));
    assert_eq!(
        executor.execute_direct(&tiny).await,
        Err(HarnessExecutionError::InvalidResult(
            "response_item_limit_exceeded".to_owned()
        ))
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn multi_outcome_executor_rejects_missing_extra_and_cross_outcome_fields() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started_with_multiple_outcomes().await else {
        return;
    };
    let invocation = fixture.pending_invocation().await;
    let executor = BuiltinHarnessPlugin::new(builtin_profile(), fixture.model.clone());

    for (outcome, fields) in [
        ("ok", json!({})),
        ("ok", json!({"reason": "belongs to unavailable"})),
        ("ok", json!({"answer": "done", "reason": "extra"})),
        ("unavailable", json!({"answer": "belongs to ok"})),
    ] {
        fixture.model.complete(outcome, fields);
        assert!(matches!(
            executor.execute_direct(&invocation).await,
            Err(HarnessExecutionError::InvalidResult(_))
        ));
    }
    fixture.cleanup().await;
}

#[tokio::test]
async fn multi_outcome_dispatcher_converts_cross_outcome_fields_to_harness_failure() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started_with_multiple_outcomes().await else {
        return;
    };
    let command = fixture.pending_command().await;
    fixture
        .model
        .complete("ok", json!({"reason": "belongs to unavailable"}));

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let failed = fixture.current().await;
    assert_eq!(failed.lifecycle, TaskLifecycle::Failed);
    assert_eq!(failed.operational_status, OperationalStatus::Finished);
    assert_eq!(failed.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.command_state(&command.id).await, "completed");
    let failure: Value = sqlx::query_scalar("SELECT failure FROM user_tasks WHERE id = $1")
        .bind(fixture.task_id.as_uuid())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(failure["code"], "harness_failed");
}

#[tokio::test]
async fn bounded_executor_enforces_turn_tool_and_active_time_limits() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let invocation = fixture.pending_invocation().await;
    let executor = BuiltinHarnessPlugin::new(builtin_profile(), fixture.model.clone());

    let calls = fixture.model.calls();
    let mut no_turns = invocation.clone();
    no_turns.limits.model_turns_remaining = 0;
    assert_eq!(
        executor.execute_direct(&no_turns).await,
        Err(HarnessExecutionError::Failed(
            "active_execution_limit_exceeded".to_owned()
        ))
    );
    assert_eq!(fixture.model.calls(), calls);

    let mut no_tools = invocation.clone();
    no_tools.limits.tool_calls_per_response = 0;
    fixture.model.respond_with(json!({
        "type":"function_call", "call_id":"tool-1", "name":"docs/search", "arguments":{}
    }));
    assert_eq!(
        executor.execute_direct(&no_tools).await,
        Err(HarnessExecutionError::InvalidResult(
            "tool_call_limit_exceeded".to_owned()
        ))
    );

    let mut timed = invocation;
    timed.limits.active_execution_millis_remaining = 1;
    fixture.model.on_request(|_| async {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        Ok(ModelResponse { items: vec![] })
    });
    assert_eq!(
        executor.execute_direct(&timed).await,
        Err(HarnessExecutionError::Failed(
            "active_execution_limit_exceeded".to_owned()
        ))
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn model_and_validation_failures_definitively_fail_the_task_without_goal_authority() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    for mode in ["model", "invalid", "timeout"] {
        let Some(fixture) = BridgeFixture::started().await else {
            return;
        };
        let command = fixture.pending_command().await;
        match mode {
            "model" => fixture
                .model
                .on_request(|_| async { Err(ModelError::Unavailable("offline".to_owned())) }),
            "invalid" => fixture.model.complete("ok", json!({"answer": 7})),
            "timeout" => fixture.model.on_request(|_| async {
                tokio::time::sleep(std::time::Duration::from_secs(31)).await;
                Ok(ModelResponse { items: vec![] })
            }),
            _ => unreachable!(),
        }
        if mode == "timeout" {
            sqlx::query(
                "UPDATE agent_commands SET payload = jsonb_set(payload, \
                 '{limits,activeExecutionMillisRemaining}', '1'::jsonb), \
                 payload_digest = $1 WHERE id = $2",
            )
            .bind("temporarily-recomputed-below")
            .bind(command.id.as_uuid())
            .execute(&fixture.pool)
            .await
            .unwrap();
            let payload: Value =
                sqlx::query_scalar("SELECT payload FROM agent_commands WHERE id = $1")
                    .bind(command.id.as_uuid())
                    .fetch_one(&fixture.pool)
                    .await
                    .unwrap();
            let parsed: InvokeHarness = serde_json::from_value(payload).unwrap();
            let digest = canonical_sha256(&parsed).unwrap();
            sqlx::query("UPDATE agent_commands SET payload_digest = $1 WHERE id = $2")
                .bind(digest)
                .bind(command.id.as_uuid())
                .execute(&fixture.pool)
                .await
                .unwrap();
        }

        assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
        let view = fixture.current().await;
        assert_eq!(view.lifecycle, TaskLifecycle::Failed, "mode={mode}");
        assert_eq!(view.operational_status, OperationalStatus::Finished);
        assert_eq!(view.goal_phase, GoalPhase::Active);
        assert_eq!(
            fixture.command_state(&command.id).await,
            "completed",
            "mode={mode}"
        );
        let failure: Value = sqlx::query_scalar("SELECT failure FROM user_tasks WHERE id = $1")
            .bind(fixture.task_id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(failure["code"], "harness_failed");
        assert_eq!(fixture.legacy_run_count().await, 0, "mode={mode}");
        assert_eq!(fixture.legacy_frame_count().await, 0, "mode={mode}");
    }
}

#[tokio::test]
async fn request_approval_is_failed_unavailable_without_calling_the_model_again() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started_with_approval().await else {
        return;
    };
    fixture.model.complete("needs_approval", json!({}));
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    assert_eq!(fixture.model.calls(), 1);
    let approval = fixture.pending_command().await;
    assert!(matches!(
        approval.payload,
        KernelCommandPayload::RequestApproval(_)
    ));

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Failed);
    assert_eq!(view.operational_status, OperationalStatus::Finished);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.command_state(&approval.id).await, "failed");
    assert_eq!(fixture.model.calls(), 1);
    assert_eq!(fixture.legacy_run_count().await, 0);
    assert_eq!(fixture.legacy_frame_count().await, 0);
    assert!(
        fixture
            .event_kinds()
            .await
            .contains(&"runtime.command_unavailable".to_owned())
    );
}

#[tokio::test]
async fn duplicate_and_foreign_claimed_ticks_never_duplicate_model_work() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    fixture.model.complete("ok", json!({"answer": "done"}));
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(fixture.model.calls(), 1);

    let Some(foreign) = BridgeFixture::started().await else {
        return;
    };
    let pending = foreign.pending_command().await;
    foreign
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: foreign.session_id.clone(),
            task_id: foreign.task_id.clone(),
            command_id: pending.id.clone(),
            expected_command_revision: pending.revision,
            claimant: "another-runtime".to_owned(),
            idempotency_key: IdempotencyKey::new("another-runtime-claim").unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(foreign.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(foreign.model.calls(), 0);
    assert_eq!(foreign.command_state(&pending.id).await, "claimed");
}

#[tokio::test]
async fn restart_resumes_the_builtin_claim_with_the_same_key_and_one_result() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let pending = fixture.pending_command().await;
    let claim_key = format!("harness-dispatcher-v1:{}", pending.id.as_uuid());
    fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session_id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: pending.id.clone(),
            expected_command_revision: pending.revision,
            claimant: "harness-dispatcher-v1".to_owned(),
            idempotency_key: IdempotencyKey::new(claim_key.clone()).unwrap(),
        })
        .await
        .unwrap();
    let restarted_kernel = Arc::new(AgentKernel::new(
        Arc::clone(&fixture.store),
        Arc::new(kiteframe::KiteAgentRuntime::new(RuntimeCeilings::default())),
        Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&fixture.store))),
    ));
    fixture.model.complete("ok", json!({"answer": "done"}));
    let profile = builtin_profile();
    let plugin = Arc::new(BuiltinHarnessPlugin::new(
        profile.clone(),
        fixture.model.clone(),
    ));
    let mut registry = HarnessRegistry::new();
    registry.register(plugin).await.unwrap();
    let restarted = restarted_kernel
        .with_harness_registry(Arc::new(registry), Arc::new(DenyAllHarnessApprovals));

    assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
    assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(fixture.model.calls(), 1);
    let row = sqlx::query(
        "SELECT state, claimant, claim_idempotency_key FROM agent_commands WHERE id = $1",
    )
    .bind(pending.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("state"), "completed");
    assert_eq!(row.get::<String, _>("claimant"), "harness-dispatcher-v1");
    assert_eq!(row.get::<String, _>("claim_idempotency_key"), claim_key);
    assert_eq!(fixture.evidence().await.len(), 1);
    let terminal_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM harness_invocations WHERE command_id = $1 AND terminal = TRUE",
    )
    .bind(pending.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(terminal_count, 1);
    let committed_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_task_events WHERE task_id = $1 \
         AND kind = 'runtime.transition_committed'",
    )
    .bind(fixture.task_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(committed_events, 1);
}

#[tokio::test]
async fn paused_work_is_not_claimed_or_sent_to_the_model() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let pending = fixture.pending_command().await;
    let current = fixture.current().await;
    fixture
        .kernel
        .pause_task(
            &fixture.principal,
            PauseUserTask {
                session_id: fixture.session_id.clone(),
                task_id: fixture.task_id.clone(),
                expected_task_revision: current.task_revision,
                expected_goal_revision: current.goal_revision,
                reason: "Pause before dispatch.".to_owned(),
                idempotency_key: IdempotencyKey::new("pause-before-dispatch").unwrap(),
            },
        )
        .await
        .unwrap();

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(fixture.model.calls(), 0);
    assert_eq!(fixture.command_state(&pending.id).await, "pending");
}

#[tokio::test]
async fn blocked_goal_is_not_redispatched_to_the_model() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started_with_blocking_outcome().await else {
        return;
    };
    fixture
        .model
        .complete("unavailable", json!({"reason": "GitHub is unavailable"}));

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let blocked = fixture.current().await;
    assert_eq!(blocked.lifecycle, TaskLifecycle::Running);
    assert_eq!(blocked.operational_status, OperationalStatus::Blocked);
    assert_eq!(blocked.goal_phase, GoalPhase::Blocked);
    assert_eq!(fixture.model.calls(), 1);

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(fixture.model.calls(), 1);
}

#[tokio::test]
async fn cancellation_during_model_execution_preserves_authority_for_either_terminal_race_winner() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        return;
    };
    let command = fixture.pending_command().await;
    let before_snapshot = fixture.snapshot_fingerprint().await;
    let before = fixture.current().await;
    fixture.model.on_request({
        let kernel = fixture.kernel.clone();
        let principal = fixture.principal.clone();
        let session_id = fixture.session_id.clone();
        let task_id = fixture.task_id.clone();
        move |_| {
            let kernel = kernel.clone();
            let principal = principal.clone();
            let session_id = session_id.clone();
            let task_id = task_id.clone();
            async move {
                kernel
                    .cancel_task(
                        &principal,
                        CancelUserTask {
                            session_id,
                            task_id: task_id.clone(),
                            expected_task_revision: before.task_revision,
                            expected_goal_revision: before.goal_revision,
                            reason: Some("Cancelled during model execution.".to_owned()),
                            idempotency_key: IdempotencyKey::new("cancel-during-model").unwrap(),
                        },
                    )
                    .await
                    .unwrap();
                Ok(ModelResponse {
                    items: vec![json!({
                        "type":"function_call", "call_id":"done-late", "name":"complete_task",
                        "arguments":{"outcome":"ok", "fields":{"answer":"too late"}}
                    })],
                })
            }
        }
    });

    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let after = fixture.current().await;
    assert_eq!(after.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(after.goal_phase, GoalPhase::Active);
    assert_eq!(fixture.command_state(&command.id).await, "cancelled");
    let harness_state: String =
        sqlx::query_scalar("SELECT state FROM harness_invocations WHERE command_id = $1")
            .bind(command.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(harness_state, "cancelled");
    assert_eq!(fixture.evidence().await.len(), 0);
    assert_eq!(fixture.snapshot_fingerprint().await, before_snapshot);
    let after_cancel = non_command_fingerprint(&fixture.pool, &fixture.task_id).await;

    let audited = sqlx::query(
        "SELECT revision, completion_idempotency_key, completion_request_digest, result, failure \
         FROM agent_commands WHERE id = $1",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(audited.get::<i64, _>("revision"), 3);
    let completion_key = audited
        .get::<Option<String>, _>("completion_idempotency_key")
        .unwrap();
    let invocation_id: uuid::Uuid =
        sqlx::query_scalar("SELECT invocation_id FROM harness_invocations WHERE command_id = $1")
            .bind(command.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert!(
        audited
            .get::<Option<String>, _>("completion_request_digest")
            .is_some()
    );
    match completion_key.as_str() {
        key if key == format!("harness-result:{invocation_id}") => {
            assert!(audited.get::<Option<Value>, _>("failure").is_none());
            let result = audited.get::<Option<Value>, _>("result").unwrap();
            let event: RuntimeEvent = serde_json::from_value(result).unwrap();
            let RuntimeEvent::HarnessResult {
                event_id,
                command_key,
                outcome,
                fields,
                evidence_id,
                history,
            } = event
            else {
                panic!("late audit must retain the harness result")
            };
            assert_eq!(outcome, "ok");
            assert_eq!(fields, json!({"answer": "too late"}));
            let replay = CompleteAgentCommand {
                session_id: fixture.session_id.clone(),
                task_id: fixture.task_id.clone(),
                command_id: command.id.clone(),
                expected_command_revision: 1,
                idempotency_key: IdempotencyKey::new(completion_key).unwrap(),
                event: RuntimeEvent::HarnessResult {
                    event_id: event_id.clone(),
                    command_key: command_key.clone(),
                    outcome: outcome.clone(),
                    fields: fields.clone(),
                    evidence_id: evidence_id.clone(),
                    history: history.clone(),
                },
            };
            fixture
                .store
                .audit_late_agent_command_completion(&fixture.principal, &replay)
                .await
                .unwrap();
            let command_after_replay: Value =
                sqlx::query_scalar("SELECT to_jsonb(c) FROM agent_commands c WHERE c.id = $1")
                    .bind(command.id.as_uuid())
                    .fetch_one(&fixture.pool)
                    .await
                    .unwrap();

            let mut conflicting = replay;
            conflicting.event = RuntimeEvent::HarnessResult {
                event_id,
                command_key,
                outcome,
                fields: json!({"answer": "conflicting late result"}),
                evidence_id,
                history,
            };
            assert!(matches!(
                fixture
                    .store
                    .audit_late_agent_command_completion(&fixture.principal, &conflicting)
                    .await,
                Err(StoreError::IdempotencyConflict)
            ));
            let command_after_conflict: Value =
                sqlx::query_scalar("SELECT to_jsonb(c) FROM agent_commands c WHERE c.id = $1")
                    .bind(command.id.as_uuid())
                    .fetch_one(&fixture.pool)
                    .await
                    .unwrap();
            assert_eq!(command_after_conflict, command_after_replay);
        }
        key if key == format!("harness-terminal:{invocation_id}") => {
            assert!(audited.get::<Option<Value>, _>("result").is_none());
            assert_eq!(
                audited.get::<Option<Value>, _>("failure").unwrap(),
                json!({"code": "harness_cancelled"})
            );
        }
        other => panic!("unexpected cancellation-race terminal receipt: {other}"),
    }
    assert_eq!(
        non_command_fingerprint(&fixture.pool, &fixture.task_id).await,
        after_cancel
    );
}

#[tokio::test]
async fn cancellation_after_command_completion_preserves_the_exact_command_winner_in_the_journal() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started_with_task(
        "tools []",
        r#"on ok(answer: string) {
          confirmation = ask string "Confirm completion?"
          done confirmation
        }"#,
        false,
    )
    .await
    else {
        eprintln!("SKIP command-winner cancellation race: DATABASE_URL is absent");
        return;
    };
    let command = fixture.pending_command().await;
    fixture.model.complete("ok", json!({"answer": "winner"}));
    let barrier = Arc::new(HarnessDispatcherTestBarrier::new(
        HarnessDispatcherTestBarrierPoint::AfterCommandTerminal,
    ));
    let dispatcher = Arc::new(fixture.dispatcher.with_test_barrier(barrier.clone()));
    let dispatch = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.tick_agent_commands(8).await }
    });
    barrier.wait_until_entered().await;

    let receipt_before: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('state', state, 'revision', revision, \
         'key', completion_idempotency_key, 'digest', completion_request_digest, \
         'result', result, 'failure', failure) FROM agent_commands WHERE id = $1",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(receipt_before["state"], "completed");
    let journal_before: bool =
        sqlx::query_scalar("SELECT terminal FROM harness_invocations WHERE command_id = $1")
            .bind(command.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert!(!journal_before);

    let current = fixture
        .store
        .get_agent_execution(fixture.session_id.clone(), fixture.task_id.clone())
        .await
        .unwrap();
    fixture
        .kernel
        .cancel_task(
            &fixture.principal,
            CancelUserTask {
                session_id: fixture.session_id.clone(),
                task_id: fixture.task_id.clone(),
                expected_task_revision: current.task_revision,
                expected_goal_revision: current.goal_revision,
                reason: Some("Cancel after the command winner committed.".to_owned()),
                idempotency_key: IdempotencyKey::new(format!(
                    "cancel-after-command-winner-{}",
                    fixture.task_id.as_uuid()
                ))
                .unwrap(),
            },
        )
        .await
        .unwrap();

    let journal_after_cancel: Value = sqlx::query_scalar(
        "SELECT terminal_payload FROM harness_invocations WHERE command_id = $1",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(journal_after_cancel["type"], "completed");
    assert_eq!(journal_after_cancel["fields"]["answer"], "winner");
    barrier.release();
    assert_eq!(dispatch.await.unwrap().unwrap(), 1);

    let receipt_after: Value = sqlx::query_scalar(
        "SELECT jsonb_build_object('state', state, 'revision', revision, \
         'key', completion_idempotency_key, 'digest', completion_request_digest, \
         'result', result, 'failure', failure) FROM agent_commands WHERE id = $1",
    )
    .bind(command.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(receipt_after, receipt_before);
    let evidence_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM goal_evidence WHERE task_id = $1")
            .bind(fixture.task_id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(evidence_count, 1);
}

#[tokio::test]
async fn cancellation_during_model_or_tool_failure_audits_only_command_failure_metadata() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    for (mode, expected_code) in [
        ("model", "harness_failed"),
        ("tool", "runtime_command_unavailable"),
    ] {
        let Some(fixture) = BridgeFixture::started_with_declared_tool().await else {
            return;
        };
        let command = fixture.pending_command().await;
        let before = fixture.current().await;
        fixture.model.on_request({
            let kernel = fixture.kernel.clone();
            let principal = fixture.principal.clone();
            let session_id = fixture.session_id.clone();
            let task_id = fixture.task_id.clone();
            move |_| {
                let kernel = kernel.clone();
                let principal = principal.clone();
                let session_id = session_id.clone();
                let task_id = task_id.clone();
                async move {
                    kernel
                        .cancel_task(
                            &principal,
                            CancelUserTask {
                                session_id,
                                task_id: task_id.clone(),
                                expected_task_revision: before.task_revision,
                                expected_goal_revision: before.goal_revision,
                                reason: Some("Cancelled during failed model work.".to_owned()),
                                idempotency_key: IdempotencyKey::new(format!(
                                    "cancel-during-{mode}"
                                ))
                                .unwrap(),
                            },
                        )
                        .await
                        .unwrap();
                    if mode == "model" {
                        Err(ModelError::Unavailable("offline after cancel".to_owned()))
                    } else {
                        Ok(ModelResponse {
                            items: vec![json!({
                                "type": "function_call",
                                "call_id": "late-tool",
                                "name": "docs/search",
                                "arguments": {}
                            })],
                        })
                    }
                }
            }
        });

        assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
        assert_eq!(fixture.current().await.lifecycle, TaskLifecycle::Cancelled);
        let audited = sqlx::query(
            "SELECT state, revision, result, failure FROM agent_commands WHERE id = $1",
        )
        .bind(command.id.as_uuid())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(audited.get::<String, _>("state"), "cancelled");
        assert_eq!(audited.get::<i64, _>("revision"), 3);
        assert!(audited.get::<Option<Value>, _>("result").is_none());
        let failure = audited.get::<Option<Value>, _>("failure").unwrap();
        assert!(
            failure == json!({"code": expected_code})
                || failure == json!({"code":"harness_cancelled"}),
            "the cancellation/late-failure race must retain one closed audit code"
        );
        assert_eq!(fixture.evidence().await.len(), 0);
    }
}

#[tokio::test]
async fn owned_pre_accept_store_failures_consume_the_attempt_without_stranding_started() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    for (point, attempt_limit, terminal) in [
        (HarnessDispatcherTestFaultPoint::WorkspaceLoad, 3, false),
        (HarnessDispatcherTestFaultPoint::EventLoad, 3, false),
        (HarnessDispatcherTestFaultPoint::EventPersistence, 3, false),
        (HarnessDispatcherTestFaultPoint::RegistryBoundary, 1, true),
    ] {
        let Some(fixture) = BridgeFixture::started().await else {
            eprintln!("SKIP owned reservation reconciliation: DATABASE_URL is absent");
            return;
        };
        let command = fixture.pending_command().await;
        fixture
            .model
            .complete("ok", json!({"answer":"one owned retry"}));
        let fault = Arc::new(HarnessDispatcherTestFault::new([point]));
        let dispatcher =
            builtin_dispatcher_with_fault(&fixture, fault.clone(), attempt_limit).await;

        assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 1);
        assert!(fault.fired(point));
        let reconciled = fixture
            .store
            .harness_invocation_for_command_revision(&command.id, command.revision + 1)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(reconciled.state, HarnessInvocationState::Started);
        assert_eq!(fixture.model.calls(), 0);

        if terminal {
            assert_eq!(reconciled.state, HarnessInvocationState::Failed);
            assert!(reconciled.terminal);
            assert_eq!(
                reconciled.terminal_payload,
                Some(json!({
                    "type":"failed",
                    "code":"harness_failed",
                    "detail":"harness pre-accept attempt budget exhausted",
                }))
            );
            assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 0);
            assert_eq!(fixture.model.calls(), 0);
        } else {
            assert_eq!(reconciled.state, HarnessInvocationState::Prepared);
            assert_eq!(reconciled.attempt, 1);
            assert!(!reconciled.terminal);
            assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 1);
            assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 0);
            assert_eq!(fixture.model.calls(), 1);
            let completed = fixture
                .store
                .harness_invocation_for_command_revision(&command.id, command.revision + 1)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(completed.state, HarnessInvocationState::Completed);
            assert_eq!(completed.attempt, 1);
        }
    }
}

#[tokio::test]
async fn owned_post_accept_poll_failure_cancels_lifecycle_and_settles_outcome_unknown_once() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        eprintln!("SKIP owned accepted reconciliation: DATABASE_URL is absent");
        return;
    };
    let command = fixture.pending_command().await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    fixture.model.on_request(move |_| {
        let entered_tx = entered_tx.clone();
        async move {
            if let Some(entered_tx) = entered_tx.lock().unwrap().take() {
                let _ = entered_tx.send(());
            }
            std::future::pending::<Result<ModelResponse, ModelError>>().await
        }
    });
    let fault = Arc::new(HarnessDispatcherTestFault::new_disarmed([
        HarnessDispatcherTestFaultPoint::CancellationPoll,
    ]));
    let dispatcher = Arc::new(builtin_dispatcher_with_fault(&fixture, fault.clone(), 3).await);
    let tick = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.tick_agent_commands(8).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered_rx)
        .await
        .expect("accepted model call starts")
        .expect("accepted model entry sender remains live");
    fault.arm();

    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), tick)
            .await
            .expect("owned lifecycle cancellation is bounded")
            .expect("owned dispatcher joins")
            .unwrap(),
        1
    );
    assert!(fault.fired(HarnessDispatcherTestFaultPoint::CancellationPoll));
    let reconciled = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, command.revision + 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reconciled.state, HarnessInvocationState::OutcomeUnknown);
    assert!(reconciled.terminal);
    assert_eq!(
        reconciled.terminal_payload,
        Some(json!({
            "type":"outcome_unknown",
            "code":"harness_outcome_unknown",
            "detail":"harness terminal outcome is unknown",
        }))
    );
    assert_eq!(fixture.model.calls(), 1);
    assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 0);
    assert_eq!(fixture.model.calls(), 1);
}

#[tokio::test]
async fn principal_cancellation_remains_authoritative_during_owned_error_reconciliation() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        eprintln!("SKIP owned cancellation reconciliation: DATABASE_URL is absent");
        return;
    };
    let command = fixture.pending_command().await;
    let fault = Arc::new(HarnessDispatcherTestFault::new([
        HarnessDispatcherTestFaultPoint::WorkspaceLoad,
    ]));
    let barrier = Arc::new(HarnessDispatcherTestBarrier::new(
        HarnessDispatcherTestBarrierPoint::BeforeOwnedReconciliation,
    ));
    let dispatcher = Arc::new(
        builtin_dispatcher_with_fault(&fixture, fault, 3)
            .await
            .with_test_barrier(barrier.clone()),
    );
    let tick = tokio::spawn({
        let dispatcher = dispatcher.clone();
        async move { dispatcher.tick_agent_commands(8).await }
    });
    barrier.wait_until_entered().await;
    let current = fixture.current().await;
    fixture
        .kernel
        .cancel_task(
            &fixture.principal,
            CancelUserTask {
                session_id: fixture.session_id.clone(),
                task_id: fixture.task_id.clone(),
                expected_task_revision: current.task_revision,
                expected_goal_revision: current.goal_revision,
                reason: Some("Cancel while owned reservation is reconciling.".to_owned()),
                idempotency_key: IdempotencyKey::new(format!(
                    "cancel-owned-reconciliation-{}",
                    Uuid::now_v7()
                ))
                .unwrap(),
            },
        )
        .await
        .unwrap();
    barrier.release();

    assert_eq!(tick.await.unwrap().unwrap(), 1);
    let reconciled = fixture
        .store
        .harness_invocation_for_command_revision(&command.id, command.revision + 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reconciled.state, HarnessInvocationState::Cancelled);
    assert!(reconciled.terminal);
    assert_eq!(fixture.command_state(&command.id).await, "cancelled");
    assert_eq!(fixture.current().await.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(fixture.model.calls(), 0);
    assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 0);
}

#[tokio::test]
async fn harness_restart_reuses_pre_accept_invocation_at_the_next_ordinal() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        eprintln!("SKIP harness restart recovery: DATABASE_URL is absent");
        return;
    };
    let pending = fixture.pending_command().await;
    let claimed = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session_id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: pending.id.clone(),
            expected_command_revision: pending.revision,
            claimant: "harness-dispatcher-v1".to_owned(),
            idempotency_key: IdempotencyKey::new(format!(
                "harness-dispatcher-v1:{}",
                pending.id.as_uuid()
            ))
            .unwrap(),
        })
        .await
        .unwrap();
    let profile = builtin_profile();
    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    let prepared = fixture
        .store
        .prepare_harness_invocation(PrepareHarnessInvocation {
            invocation_id: &invocation_id,
            command_id: &claimed.id,
            command_revision: claimed.revision,
            profile_id: &profile.id,
            profile_digest: &profile.digest().unwrap(),
            payload_digest: &claimed.payload_digest,
        })
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation_id, prepared.attempt)
        .await
        .unwrap();

    let report = fixture
        .dispatcher
        .recover_harness_invocations(8)
        .await
        .unwrap();
    assert_eq!(report.retryable_pre_accept, 1);
    assert_eq!(report.outcome_unknown, 0);
    assert_eq!(report.terminal, 0);
    let recovered = fixture
        .store
        .harness_invocation_for_command_revision(&claimed.id, claimed.revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.invocation_id, invocation_id);
    assert_eq!(recovered.state, HarnessInvocationState::Prepared);
    assert_eq!(recovered.attempt, 1, "Started(0) is durably consumed");

    fixture.model.complete("ok", json!({"answer": "recovered"}));
    assert_eq!(fixture.dispatcher.tick_agent_commands(8).await.unwrap(), 1);
    let completed = fixture
        .store
        .harness_invocation_for_command_revision(&claimed.id, claimed.revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.invocation_id, invocation_id);
    assert_eq!(completed.attempt, 1);
    assert_eq!(fixture.model.calls(), 1);
}

#[tokio::test]
async fn harness_restart_marks_accepted_unknown_and_fails_only_the_bounded_command() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        eprintln!("SKIP accepted harness restart recovery: DATABASE_URL is absent");
        return;
    };
    let pending = fixture.pending_command().await;
    let claimed = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session_id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: pending.id.clone(),
            expected_command_revision: pending.revision,
            claimant: "harness-dispatcher-v1".to_owned(),
            idempotency_key: IdempotencyKey::new(format!(
                "harness-dispatcher-v1:{}",
                pending.id.as_uuid()
            ))
            .unwrap(),
        })
        .await
        .unwrap();
    let profile = builtin_profile();
    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    let prepared = fixture
        .store
        .prepare_harness_invocation(PrepareHarnessInvocation {
            invocation_id: &invocation_id,
            command_id: &claimed.id,
            command_revision: claimed.revision,
            profile_id: &profile.id,
            profile_digest: &profile.digest().unwrap(),
            payload_digest: &claimed.payload_digest,
        })
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation_id, prepared.attempt)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation_id, prepared.attempt)
        .await
        .unwrap();

    let report = fixture
        .dispatcher
        .recover_harness_invocations(8)
        .await
        .unwrap();
    assert_eq!(report.retryable_pre_accept, 0);
    assert_eq!(report.outcome_unknown, 1);
    assert_eq!(report.terminal, 0);
    assert_eq!(fixture.command_state(&claimed.id).await, "completed");
    assert_eq!(fixture.model.calls(), 0);
    assert!(fixture.evidence().await.is_empty());
    let execution = fixture.current().await;
    assert_eq!(execution.goal_phase, GoalPhase::Active);
    let recovered = fixture
        .store
        .harness_invocation_for_command_revision(&claimed.id, claimed.revision)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.state, HarnessInvocationState::OutcomeUnknown);
    assert_eq!(
        recovered.terminal_payload,
        Some(json!({
            "type":"outcome_unknown",
            "code":"harness_outcome_unknown",
            "detail":"harness terminal outcome is unknown",
        }))
    );
}

#[tokio::test]
async fn harness_restart_replays_committed_terminal_into_kernel_exactly_once() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    let Some(fixture) = BridgeFixture::started().await else {
        eprintln!("SKIP committed harness terminal recovery: DATABASE_URL is absent");
        return;
    };
    let pending = fixture.pending_command().await;
    let claimed = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session_id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: pending.id.clone(),
            expected_command_revision: pending.revision,
            claimant: "harness-dispatcher-v1".to_owned(),
            idempotency_key: IdempotencyKey::new(format!(
                "harness-dispatcher-v1:{}",
                pending.id.as_uuid()
            ))
            .unwrap(),
        })
        .await
        .unwrap();
    let profile = builtin_profile();
    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    fixture
        .store
        .prepare_harness_invocation(PrepareHarnessInvocation {
            invocation_id: &invocation_id,
            command_id: &claimed.id,
            command_revision: claimed.revision,
            profile_id: &profile.id,
            profile_digest: &profile.digest().unwrap(),
            payload_digest: &claimed.payload_digest,
        })
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation_id, 0)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation_id, 0)
        .await
        .unwrap();
    force_legacy_terminal_journal(
        &fixture.pool,
        &invocation_id,
        "completed",
        &format!("harness-terminal:{}", invocation_id.as_uuid()),
        json!({
            "type":"completed",
            "outcome":"ok",
            "fields":{"answer":"committed-before-crash"},
            "history":[],
        }),
    )
    .await;

    let report = fixture
        .dispatcher
        .recover_harness_invocations(8)
        .await
        .unwrap();
    assert_eq!(report.terminal, 1);
    assert_eq!(report.retryable_pre_accept, 0);
    assert_eq!(report.outcome_unknown, 0);
    assert_eq!(fixture.command_state(&claimed.id).await, "completed");
    assert_eq!(fixture.evidence().await.len(), 1);
    assert_eq!(fixture.model.calls(), 0);

    assert_eq!(
        fixture
            .dispatcher
            .recover_harness_invocations(8)
            .await
            .unwrap(),
        kiteframe::HarnessRecoveryReport::default()
    );
    assert_eq!(fixture.evidence().await.len(), 1);
}

#[tokio::test]
async fn fabricated_context_and_event_commands_fail_recovery_integrity_before_dispatch() {
    let _guard = BRIDGE_TEST_LOCK.lock().await;
    for (kind, payload) in [
        (
            "request_context",
            KernelCommandPayload::RequestContext(RequestContext {
                evidence_type: "github.context".to_owned(),
                subject: json!({"repository": "acme/widgets"}),
            }),
        ),
        (
            "wait_for_event",
            KernelCommandPayload::WaitForEvent(WaitForEvent {
                event_type: "github.changed.v1".to_owned(),
                correlation: json!({"repository": "acme/widgets"}),
            }),
        ),
    ] {
        let Some(fixture) = BridgeFixture::started().await else {
            return;
        };
        let command = fixture.pending_command().await;
        let original = sqlx::query(
            "SELECT command_kind, payload, payload_digest FROM agent_commands WHERE id = $1",
        )
        .bind(command.id.as_uuid())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        let value = serde_json::to_value(&payload).unwrap();
        let digest = canonical_sha256(&payload).unwrap();
        sqlx::query(
            "UPDATE agent_commands SET command_kind = $1, payload = $2, payload_digest = $3 \
             WHERE id = $4",
        )
        .bind(kind)
        .bind(value)
        .bind(digest)
        .bind(command.id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();

        assert!(matches!(
            fixture.store.recover_agent_commands(8).await,
            Err(StoreError::RuntimeStateInvalid(_))
        ));
        assert_eq!(fixture.model.calls(), 0);
        assert_eq!(fixture.command_state(&command.id).await, "pending");
        sqlx::query(
            "UPDATE agent_commands SET command_kind = $1, payload = $2, payload_digest = $3 \
             WHERE id = $4",
        )
        .bind(original.get::<String, _>("command_kind"))
        .bind(original.get::<Value, _>("payload"))
        .bind(original.get::<String, _>("payload_digest"))
        .bind(command.id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();
        fixture.cleanup().await;
    }
}
