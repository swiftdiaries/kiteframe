use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, Engine, EngineConfig,
    IdempotencyKey, InProcessToolRegistry, ModelClient, ModelError, ModelRequest, ModelResponse,
    AnswerQuestion, PermissionChoice, PolicyDecision, PolicyRequest, PostgresStore, PrincipalId, RecoverySupport,
    ResolvePermission, SessionAuthorizer, StartRun, ToolCall, ToolCatalogSnapshot, ToolError, ToolStatus,
    ToolResolver, TrustedTool, bind_tool,
};
use serde_json::{Value, json};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct ScriptedModel(Mutex<VecDeque<Vec<Value>>>);

#[async_trait]
impl ModelClient for ScriptedModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse { items: self.0.lock().unwrap().pop_front().expect("scripted response") })
    }
}

struct AllowAll;

#[async_trait]
impl SessionAuthorizer for AllowAll {
    async fn authorize(&self, _request: &PolicyRequest) -> Result<PolicyDecision, kiteframe::PolicyError> {
        Ok(PolicyDecision::Allow)
    }
}

struct RequirePermission;

#[async_trait]
impl SessionAuthorizer for RequirePermission {
    async fn authorize(&self, _request: &PolicyRequest) -> Result<PolicyDecision, kiteframe::PolicyError> {
        Ok(PolicyDecision::NeedsPermission)
    }
}

struct SearchTool { descriptor: CatalogTool }

#[async_trait]
impl TrustedTool for SearchTool {
    fn descriptor(&self) -> &CatalogTool { &self.descriptor }
    async fn invoke(&self, _call: ToolCall) -> Result<Value, ToolError> { Ok(json!({"result":"found"})) }
    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> { Ok(ToolStatus::Succeeded { result: json!({}) }) }
}

#[tokio::test]
async fn permitted_model_tool_call_replays_output_then_completes() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else { return; };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store.create_session(CreateSession {
        principal: PrincipalId::new(format!("runtime-{}", Uuid::now_v7())).unwrap(),
        working_directory: "/work".to_owned(), retention_until: OffsetDateTime::now_utc() + Duration::days(1),
    }).await.expect("session");
    let descriptor = CatalogTool::new("docs/search", "docs", "search", ClosedSchema::object("input", BTreeMap::new()), ClosedSchema::object("output", BTreeMap::new()), BTreeSet::new(), EffectKind::ReadOnly, RecoverySupport::NotRequired).unwrap();
    let registry = Arc::new(InProcessToolRegistry::new());
    registry.register(bind_tool(Arc::new(SearchTool { descriptor: descriptor.clone() }))).unwrap();
    let catalog = registry.snapshot(session.id.clone(), Uuid::now_v7(), &BTreeSet::from(["docs/search".to_owned()]), &BTreeMap::from([("docs/search".to_owned(), descriptor.descriptor_digest().to_owned())])).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("w{}", Uuid::now_v7().simple());
    let source = format!("workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok(answer: string) {{ done answer }} }}");
    let workflow = compile(&source, &catalog, &Limits::default()).unwrap();
    let definition = store.publish(session.id.clone(), &workflow, &source).await.unwrap();
    let run = store.start_run(StartRun { session_id: session.id.clone(), workflow_name: definition.name, version: Some(definition.version), input: json!({}), idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap() }).await.unwrap();
    assert_eq!(store.runnable_frames_for_run(run.id.clone(), 16).await.unwrap().into_iter().find(|frame| frame.id == run.root_frame_id).unwrap().session_id, session.id);
    registry.bound_tool(session.id.clone(), "docs/search", descriptor.descriptor_digest()).unwrap();
    let model = Arc::new(ScriptedModel(Mutex::new(VecDeque::from([
        vec![json!({"type":"function_call","call_id":"call-1","name":"docs/search","arguments":{}})],
        vec![json!({"type":"function_call","call_id":"complete-1","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}})],
    ]))));
    let engine = Engine { store: store.clone(), model, tools: registry, authorizer: Arc::new(AllowAll), config: EngineConfig::default() };
    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1").bind(*run.id.as_uuid()).fetch_one(store.pool()).await.unwrap().get("state");
    assert_eq!(state, "completed");
}

#[tokio::test]
async fn allow_once_resolves_and_invokes_the_pending_durable_call() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else { return; };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store.create_session(CreateSession { principal: PrincipalId::new(format!("permission-{}", Uuid::now_v7())).unwrap(), working_directory: "/work".to_owned(), retention_until: OffsetDateTime::now_utc() + Duration::days(1) }).await.unwrap();
    let descriptor = CatalogTool::new("docs/search", "docs", "search", ClosedSchema::object("input", BTreeMap::new()), ClosedSchema::object("output", BTreeMap::new()), BTreeSet::new(), EffectKind::ReadOnly, RecoverySupport::NotRequired).unwrap();
    let registry = Arc::new(InProcessToolRegistry::new());
    registry.register(bind_tool(Arc::new(SearchTool { descriptor: descriptor.clone() }))).unwrap();
    let catalog = registry.snapshot(session.id.clone(), Uuid::now_v7(), &BTreeSet::from(["docs/search".to_owned()]), &BTreeMap::from([("docs/search".to_owned(), descriptor.descriptor_digest().to_owned())])).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("p{}", Uuid::now_v7().simple());
    let source = format!("workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok(answer: string) {{ done answer }} }}");
    let workflow = compile(&source, &catalog, &Limits::default()).unwrap();
    let definition = store.publish(session.id.clone(), &workflow, &source).await.unwrap();
    let run = store.start_run(StartRun { session_id: session.id.clone(), workflow_name: definition.name, version: Some(definition.version), input: json!({}), idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap() }).await.unwrap();
    let engine = Engine { store: store.clone(), model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([
        vec![json!({"type":"function_call","call_id":"call-1","name":"docs/search","arguments":{}})],
        vec![json!({"type":"function_call","call_id":"complete-1","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}})],
    ])))), tools: registry, authorizer: Arc::new(RequirePermission), config: EngineConfig::default() };
    engine.tick_run(run.id.clone()).await.unwrap();
    let request_id: Uuid = sqlx::query("SELECT id FROM permission_requests WHERE run_id = $1").bind(*run.id.as_uuid()).fetch_one(store.pool()).await.unwrap().get("id");
    let approval_key = IdempotencyKey::new(format!("approval-{}", Uuid::now_v7())).unwrap();
    let approval = ResolvePermission { session_id: session.id.clone(), run_id: run.id.clone(), permission_request_id: kiteframe::PermissionRequestId::new(request_id), choice: PermissionChoice::AllowOnce, idempotency_key: approval_key.clone() };
    engine.resolve_permission(approval.clone()).await.unwrap();
    engine.resolve_permission(approval).await.unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1").bind(*run.id.as_uuid()).fetch_one(store.pool()).await.unwrap().get("state");
    assert_eq!(state, "completed");
}

#[tokio::test]
async fn scoped_answer_resumes_a_durable_question() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else { return; };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store.create_session(CreateSession { principal: PrincipalId::new(format!("question-{}", Uuid::now_v7())).unwrap(), working_directory: "/work".to_owned(), retention_until: OffsetDateTime::now_utc() + Duration::days(1) }).await.unwrap();
    let registry = Arc::new(InProcessToolRegistry::new());
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("q{}", Uuid::now_v7().simple());
    let source = format!("workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ask_one(question: string) {{ answer = ask string question done answer }} }}");
    let workflow = compile(&source, &catalog, &Limits::default()).unwrap();
    let definition = store.publish(session.id.clone(), &workflow, &source).await.unwrap();
    let run = store.start_run(StartRun { session_id: session.id.clone(), workflow_name: definition.name, version: Some(definition.version), input: json!({}), idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap() }).await.unwrap();
    let engine = Engine { store: store.clone(), model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([vec![json!({"type":"function_call","call_id":"complete-1","name":"complete_task","arguments":{"outcome":"ask_one","fields":{"question":"Continue?"}}})]])))), tools: registry, authorizer: Arc::new(AllowAll), config: EngineConfig::default() };
    engine.tick_run(run.id.clone()).await.unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    let question_id: Uuid = sqlx::query("SELECT id FROM questions WHERE run_id = $1").bind(*run.id.as_uuid()).fetch_one(store.pool()).await.unwrap().get("id");
    engine.answer_question(AnswerQuestion { session_id: session.id.clone(), run_id: run.id.clone(), question_id, answer: json!("yes"), idempotency_key: IdempotencyKey::new(format!("answer-{}", Uuid::now_v7())).unwrap() }).await.unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1").bind(*run.id.as_uuid()).fetch_one(store.pool()).await.unwrap().get("state");
    assert_eq!(state, "completed");
}
