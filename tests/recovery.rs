use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use kiteframe::dsl::{compile, Limits};
use kiteframe::{
    bind_tool, CatalogTool, ClosedSchema, CreateSession, EffectKind, Engine, EngineConfig,
    IdempotencyKey, InProcessToolRegistry, ModelClient, ModelError, ModelRequest, ModelResponse,
    PolicyDecision, PolicyRequest, PostgresStore, PrincipalId, RecoverySupport,
    SessionAuthorizer, StartRun, ToolCall, ToolError, ToolStatus, TrustedTool,
};
use serde_json::{json, Value};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct ScriptedModel(Mutex<VecDeque<Vec<Value>>>);

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            items: self.0.lock().expect("model lock").pop_front().expect("model response"),
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
    store.append_catalog_epoch(&catalog).await.expect("catalog epoch");
    let name = format!("recover{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [write] on ok(answer: string) {{ done answer }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store.publish(session.id.clone(), &workflow, &source).await.expect("publish");
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
        .commit_model_attempt(run.root_frame_id.clone(), attempt.attempt, attempt.revision, vec![])
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
}
