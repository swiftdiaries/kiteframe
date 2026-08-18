use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, Engine, EngineConfig, IdempotencyKey,
    InProcessToolRegistry, ModelClient, ModelError, ModelRequest, ModelResponse, PolicyDecision,
    PolicyRequest, PostgresStore, PrincipalId, RecoverySupport, RetryRecovery, SessionAuthorizer,
    SessionId, StartRun, ToolCall, ToolCatalogSnapshot, ToolError, ToolResolver, ToolStatus,
    TrustedTool, bind_tool,
};
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct ScriptedModel(Mutex<VecDeque<Vec<Value>>>);

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            items: self
                .0
                .lock()
                .expect("model lock")
                .pop_front()
                .expect("model response"),
        })
    }
}

struct AllowAll;

#[async_trait]
impl SessionAuthorizer for AllowAll {
    async fn authorize(
        &self,
        _request: &PolicyRequest,
    ) -> Result<PolicyDecision, kiteframe::PolicyError> {
        Ok(PolicyDecision::Allow)
    }
}

struct UnavailableStatusTool {
    descriptor: CatalogTool,
    invocations: Arc<AtomicUsize>,
}

struct PendingStatusTool {
    descriptor: CatalogTool,
    invocations: Arc<AtomicUsize>,
    status_polls: Arc<AtomicUsize>,
}

struct NotStartedRetryTool {
    descriptor: CatalogTool,
    invocations: Arc<AtomicUsize>,
}

struct IsolatedDatabase {
    name: String,
    admin_url: String,
    url: String,
}

impl IsolatedDatabase {
    async fn create(base_url: &str) -> Self {
        let mut admin = url::Url::parse(base_url).expect("database URL");
        admin.set_path("/postgres");
        let name = format!("kiteframe_recovery_{}", Uuid::now_v7().simple());
        let admin_url = admin.to_string();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("connect database administrator");
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&pool)
            .await
            .expect("create isolated recovery database");
        drop(pool);
        let mut database = admin;
        database.set_path(&format!("/{name}"));
        Self {
            name,
            admin_url,
            url: database.to_string(),
        }
    }

    async fn drop(self) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .expect("connect database administrator for cleanup");
        sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
            .execute(&pool)
            .await
            .expect("drop isolated recovery database");
    }
}

#[tokio::test]
async fn one_recovery_call_pages_through_more_than_256_model_attempts() {
    let Ok(base_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let database = IsolatedDatabase::create(&base_url).await;
    let store = Arc::new(PostgresStore::connect(&database.url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("recovery-page-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).expect("empty catalog");
    store.append_catalog_epoch(&catalog).await.expect("catalog");
    let name = format!("recoverpage{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok(answer: string) {{ done answer }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .expect("run");
    store
        .begin_model_attempt(run.root_frame_id.clone(), 0, json!({}))
        .await
        .expect("first model attempt");
    sqlx::query(
        "WITH new_runs AS ( \
           INSERT INTO runs (id, session_id, definition_id, start_idempotency_key, input, input_digest, state) \
           SELECT gen_random_uuid(), $1, $2, 'bulk-recovery-' || n || '-' || $3, '{}'::jsonb, 'bulk-input', 'running' \
           FROM generate_series(1, 256) AS n RETURNING id \
         ), new_frames AS ( \
           INSERT INTO frames (id, run_id, task_name, state, bindings, revision) \
           SELECT gen_random_uuid(), id, $4, 'running', '{}'::jsonb, 1 FROM new_runs RETURNING id \
         ) \
         INSERT INTO model_items (frame_id, attempt, position, direction, item) \
         SELECT id, 1, 0, 'input', '{}'::jsonb FROM new_frames",
    )
    .bind(*session.id.as_uuid())
    .bind(definition.id)
    .bind(Uuid::now_v7().to_string())
    .bind(workflow.entry.task)
    .execute(store.pool())
    .await
    .expect("bulk interrupted attempts");
    let engine = Engine {
        store,
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::new()))),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };

    let report = engine.recover().await.expect("paged recovery");
    assert_eq!(report.model_attempts_requeued, 257);
    database.drop().await;
}

#[async_trait]
impl TrustedTool for PendingStatusTool {
    fn descriptor(&self) -> &CatalogTool {
        &self.descriptor
    }

    async fn invoke(&self, _call: ToolCall) -> Result<Value, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Err(ToolError::InvocationFailed)
    }

    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        self.status_polls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolStatus::Pending)
    }
}

#[async_trait]
impl TrustedTool for UnavailableStatusTool {
    fn descriptor(&self) -> &CatalogTool {
        &self.descriptor
    }

    async fn invoke(&self, _call: ToolCall) -> Result<Value, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Err(ToolError::InvocationFailed)
    }

    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        Err(ToolError::InvocationFailed)
    }
}

#[async_trait]
impl TrustedTool for NotStartedRetryTool {
    fn descriptor(&self) -> &CatalogTool {
        &self.descriptor
    }

    async fn invoke(&self, _call: ToolCall) -> Result<Value, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }

    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        Ok(ToolStatus::NotStarted)
    }
}

#[tokio::test]
async fn direct_recovery_retry_replays_only_a_status_proven_read_only_call() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("retry-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let descriptor = CatalogTool::new(
        "docs/search",
        "docs",
        "search",
        ClosedSchema::object("input", BTreeMap::new()),
        ClosedSchema::object("output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .unwrap();
    let invocations = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(InProcessToolRegistry::new());
    tools
        .register(bind_tool(Arc::new(NotStartedRetryTool {
            descriptor: descriptor.clone(),
            invocations: invocations.clone(),
        })))
        .unwrap();
    let catalog = tools
        .snapshot(
            session.id.clone(),
            Uuid::now_v7(),
            &BTreeSet::from(["docs/search".to_owned()]),
            &BTreeMap::from([(
                "docs/search".to_owned(),
                descriptor.descriptor_digest().to_owned(),
            )]),
        )
        .unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("retry{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok(answer: string) {{ done answer }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).unwrap();
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .unwrap();
    let run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    let attempt = store
        .begin_model_attempt(run.root_frame_id.clone(), 0, json!({}))
        .await
        .unwrap();
    store
        .commit_model_attempt(
            run.root_frame_id.clone(),
            attempt.attempt,
            attempt.revision,
            vec![],
        )
        .await
        .unwrap();
    let intent = store
        .begin_tool_call(
            run.root_frame_id.clone(),
            2,
            "call-1".to_owned(),
            "docs/search".to_owned(),
            descriptor.descriptor_digest().to_owned(),
            json!({}),
            "original-effect-key".to_owned(),
        )
        .await
        .unwrap();
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::new()))),
        tools,
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };

    engine
        .retry_recovery(RetryRecovery {
            session_id: session.id.clone(),
            run_id: run.id.clone(),
            tool_call_id: intent.id,
        })
        .await
        .expect("direct user retry");

    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    let call = sqlx::query("SELECT state, idempotency_key FROM tool_calls WHERE id = $1")
        .bind(intent.id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(call.get::<String, _>("state"), "completed");
    assert_eq!(
        call.get::<String, _>("idempotency_key"),
        "original-effect-key"
    );
}

#[tokio::test]
async fn unavailable_status_durably_marks_an_effect_outcome_unknown() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("recovery-{}", Uuid::now_v7())).expect("principal"),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let descriptor = CatalogTool::new(
        "docs/write",
        "docs",
        "write",
        ClosedSchema::object("input", BTreeMap::new()),
        ClosedSchema::object("output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::Effectful,
        RecoverySupport::Status,
    )
    .expect("descriptor");
    let tools = Arc::new(InProcessToolRegistry::new());
    let invocations = Arc::new(AtomicUsize::new(0));
    tools
        .register(bind_tool(Arc::new(UnavailableStatusTool {
            descriptor: descriptor.clone(),
            invocations: invocations.clone(),
        })))
        .expect("register tool");
    let catalog = tools
        .snapshot(
            session.id.clone(),
            Uuid::now_v7(),
            &BTreeSet::from(["docs/write".to_owned()]),
            &BTreeMap::from([(
                "docs/write".to_owned(),
                descriptor.descriptor_digest().to_owned(),
            )]),
        )
        .expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let name = format!("recover{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [write] on ok(answer: string) {{ done answer }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).expect("key"),
        })
        .await
        .expect("run");
    let attempt = store
        .begin_model_attempt(run.root_frame_id.clone(), 0, json!({}))
        .await
        .expect("model intent");
    store
        .commit_model_attempt(
            run.root_frame_id.clone(),
            attempt.attempt,
            attempt.revision,
            vec![],
        )
        .await
        .expect("model output");
    store
        .begin_tool_call(
            run.root_frame_id.clone(),
            2,
            "call-1".to_owned(),
            "docs/write".to_owned(),
            descriptor.descriptor_digest().to_owned(),
            json!({}),
            "call-key-1".to_owned(),
        )
        .await
        .expect("tool intent");
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::new()))),
        tools,
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };

    let report = engine.recover().await.expect("recover");

    assert_eq!(report.outcomes_unknown, 1);
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
    let call_state: String = sqlx::query("SELECT state FROM tool_calls WHERE frame_id = $1")
        .bind(*run.root_frame_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .expect("tool call")
        .get("state");
    let frame_state: String = sqlx::query("SELECT state FROM frames WHERE id = $1")
        .bind(*run.root_frame_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .expect("frame")
        .get("state");
    let run_state: String = sqlx::query("SELECT state FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .expect("run")
        .get("state");
    assert_eq!(call_state, "outcome_unknown");
    assert_eq!(frame_state, "outcome_unknown");
    assert_eq!(run_state, "outcome_unknown");
    let events: Vec<String> =
        sqlx::query("SELECT kind FROM events WHERE run_id = $1 ORDER BY sequence")
            .bind(*run.id.as_uuid())
            .fetch_all(store.pool())
            .await
            .expect("events")
            .into_iter()
            .map(|row| row.get("kind"))
            .collect();
    assert!(events.iter().any(|kind| kind == "tool.status_check_intent"));
}

#[tokio::test]
async fn outcome_unknown_migration_retains_child_wait_state() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = PostgresStore::connect(&database_url).await.expect("store");
    let constraint: String = sqlx::query(
        "SELECT pg_get_constraintdef(c.oid) FROM pg_constraint c JOIN pg_class r ON r.oid = c.conrelid WHERE r.relname = 'frames' AND c.conname = 'frames_state_check'",
    )
    .fetch_one(store.pool())
    .await
    .expect("frame-state constraint")
    .get(0);
    assert!(constraint.contains("waiting_for_children"));
    assert!(constraint.contains("outcome_unknown"));
}

struct UnattachedResolver;

#[async_trait]
impl ToolResolver for UnattachedResolver {
    fn bound_tool(
        &self,
        session: SessionId,
        _qualified_name: &str,
        _descriptor_digest: &str,
    ) -> Result<Arc<dyn kiteframe::BoundTool>, ToolError> {
        Err(ToolError::UnknownSession(session))
    }

    async fn invoke(
        &self,
        _session: SessionId,
        _qualified_name: &str,
        _descriptor_digest: &str,
        _call: ToolCall,
    ) -> Result<Value, ToolError> {
        Err(ToolError::InvocationFailed)
    }
}

#[tokio::test]
async fn recovery_reports_sessions_that_need_fresh_mcp_attachment() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("mcp-wait-{}", Uuid::now_v7())).expect("principal"),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let descriptor = CatalogTool::new(
        "docs/write",
        "docs",
        "write",
        ClosedSchema::object("input", BTreeMap::new()),
        ClosedSchema::object("output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::Effectful,
        RecoverySupport::Status,
    )
    .expect("descriptor");
    let attached = Arc::new(InProcessToolRegistry::new());
    attached
        .register(bind_tool(Arc::new(UnavailableStatusTool {
            descriptor: descriptor.clone(),
            invocations: Arc::new(AtomicUsize::new(0)),
        })))
        .expect("register tool");
    let catalog = attached
        .snapshot(
            session.id.clone(),
            Uuid::now_v7(),
            &BTreeSet::from(["docs/write".to_owned()]),
            &BTreeMap::from([(
                "docs/write".to_owned(),
                descriptor.descriptor_digest().to_owned(),
            )]),
        )
        .expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let name = format!("mcpwait{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [write] on ok(answer: string) {{ done answer }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).expect("key"),
        })
        .await
        .expect("run");
    let attempt = store
        .begin_model_attempt(run.root_frame_id.clone(), 0, json!({}))
        .await
        .expect("model");
    store
        .commit_model_attempt(
            run.root_frame_id.clone(),
            attempt.attempt,
            attempt.revision,
            vec![],
        )
        .await
        .expect("commit");
    store
        .begin_tool_call(
            run.root_frame_id,
            2,
            "call-1".to_owned(),
            "docs/write".to_owned(),
            descriptor.descriptor_digest().to_owned(),
            json!({}),
            "call-key-1".to_owned(),
        )
        .await
        .expect("intent");
    let engine = Engine {
        store,
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::new()))),
        tools: Arc::new(UnattachedResolver),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    assert!(
        engine
            .recover()
            .await
            .expect("recover")
            .sessions_waiting_for_mcp
            >= 1
    );
}

#[tokio::test]
async fn pending_status_is_polled_again_before_retry_budget_exhaustion() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("pending-{}", Uuid::now_v7())).expect("principal"),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let descriptor = CatalogTool::new(
        "docs/write",
        "docs",
        "write",
        ClosedSchema::object("input", BTreeMap::new()),
        ClosedSchema::object("output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::Effectful,
        RecoverySupport::Status,
    )
    .expect("descriptor");
    let invocations = Arc::new(AtomicUsize::new(0));
    let status_polls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(InProcessToolRegistry::new());
    tools
        .register(bind_tool(Arc::new(PendingStatusTool {
            descriptor: descriptor.clone(),
            invocations: invocations.clone(),
            status_polls: status_polls.clone(),
        })))
        .expect("register tool");
    let catalog = tools
        .snapshot(
            session.id.clone(),
            Uuid::now_v7(),
            &BTreeSet::from(["docs/write".to_owned()]),
            &BTreeMap::from([(
                "docs/write".to_owned(),
                descriptor.descriptor_digest().to_owned(),
            )]),
        )
        .expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let name = format!("pending{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [write] on ok(answer: string) {{ done answer }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).expect("key"),
        })
        .await
        .expect("run");
    let attempt = store
        .begin_model_attempt(run.root_frame_id.clone(), 0, json!({}))
        .await
        .expect("model");
    store
        .commit_model_attempt(
            run.root_frame_id.clone(),
            attempt.attempt,
            attempt.revision,
            vec![],
        )
        .await
        .expect("commit");
    store
        .begin_tool_call(
            run.root_frame_id.clone(),
            2,
            "call-1".to_owned(),
            "docs/write".to_owned(),
            descriptor.descriptor_digest().to_owned(),
            json!({}),
            "call-key-1".to_owned(),
        )
        .await
        .expect("intent");
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::new()))),
        tools,
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    let first = engine.recover().await.expect("first recovery");
    assert_eq!(first.tools_requeued, 3);
    assert_eq!(first.outcomes_unknown, 1);
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
    assert_eq!(status_polls.load(Ordering::SeqCst), 3);
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .expect("run")
        .get("state");
    assert_eq!(state, "outcome_unknown");
}
