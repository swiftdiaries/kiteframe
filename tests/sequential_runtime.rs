use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    AnswerQuestion, CatalogTool, ClosedSchema, CreateSession, EffectKind, Engine, EngineConfig,
    IdempotencyKey, InProcessToolRegistry, ModelClient, ModelError, ModelRequest, ModelResponse,
    PermissionChoice, PolicyDecision, PolicyRequest, PostgresStore, PrincipalId, RecoverySupport,
    ResolvePermission, SessionAuthorizer, StartRun, ToolCall, ToolCatalogSnapshot, ToolError,
    ToolResolver, ToolStatus, TrustedTool, bind_tool,
};
use serde_json::{Value, json};
use sqlx::Row;
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
                .unwrap()
                .pop_front()
                .expect("scripted response"),
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

struct RequirePermission;

#[async_trait]
impl SessionAuthorizer for RequirePermission {
    async fn authorize(
        &self,
        _request: &PolicyRequest,
    ) -> Result<PolicyDecision, kiteframe::PolicyError> {
        Ok(PolicyDecision::NeedsPermission)
    }
}

struct SearchTool {
    descriptor: CatalogTool,
}

struct FailingTool {
    descriptor: CatalogTool,
}

struct ChangedResolver;

#[async_trait]
impl ToolResolver for ChangedResolver {
    fn bound_tool(
        &self,
        _session: kiteframe::SessionId,
        qualified_name: &str,
        _descriptor_digest: &str,
    ) -> Result<Arc<dyn kiteframe::BoundTool>, ToolError> {
        Err(ToolError::ToolContractChanged {
            qualified_name: qualified_name.to_owned(),
        })
    }

    async fn invoke(
        &self,
        _session: kiteframe::SessionId,
        _qualified_name: &str,
        _descriptor_digest: &str,
        _call: ToolCall,
    ) -> Result<Value, ToolError> {
        unreachable!("scheduler resolves before invoking")
    }
}

#[async_trait]
impl TrustedTool for SearchTool {
    fn descriptor(&self) -> &CatalogTool {
        &self.descriptor
    }
    async fn invoke(&self, _call: ToolCall) -> Result<Value, ToolError> {
        Ok(json!({"result":"found"}))
    }
    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        Ok(ToolStatus::Succeeded { result: json!({}) })
    }
}

#[async_trait]
impl TrustedTool for FailingTool {
    fn descriptor(&self) -> &CatalogTool {
        &self.descriptor
    }

    async fn invoke(&self, _call: ToolCall) -> Result<Value, ToolError> {
        Err(ToolError::InvocationFailed)
    }

    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        Ok(ToolStatus::Failed {
            error: json!({"code":"tool_failed"}),
        })
    }
}

#[tokio::test]
async fn permitted_model_tool_call_replays_output_then_completes() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("runtime-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
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
    let registry = Arc::new(InProcessToolRegistry::new());
    registry
        .register(bind_tool(Arc::new(SearchTool {
            descriptor: descriptor.clone(),
        })))
        .unwrap();
    let catalog = registry
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
    let name = format!("w{}", Uuid::now_v7().simple());
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
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .runnable_frames_for_run(run.id.clone(), 16)
            .await
            .unwrap()
            .into_iter()
            .find(|frame| frame.id == run.root_frame_id)
            .unwrap()
            .session_id,
        session.id
    );
    registry
        .bound_tool(
            session.id.clone(),
            "docs/search",
            descriptor.descriptor_digest(),
        )
        .unwrap();
    let model = Arc::new(ScriptedModel(Mutex::new(VecDeque::from([
        vec![
            json!({"type":"function_call","call_id":"call-1","name":"docs/search","arguments":{}}),
        ],
        vec![
            json!({"type":"function_call","call_id":"complete-1","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}}),
        ],
    ]))));
    let engine = Engine {
        store: store.clone(),
        model,
        tools: registry,
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("state");
    assert_eq!(state, "completed");
}

#[tokio::test]
async fn changed_descriptor_becomes_a_durable_tool_contract_failure() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("changed-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
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
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [descriptor.clone()]).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("changed{}", Uuid::now_v7().simple());
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
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([vec![
            json!({"type":"function_call","call_id":"changed-call","name":"docs/search","arguments":{}}),
        ]])))),
        tools: Arc::new(ChangedResolver),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    engine
        .tick_run(run.id.clone())
        .await
        .expect("scheduler survives changed descriptor");
    let error: Value = sqlx::query("SELECT error FROM tool_calls WHERE frame_id = $1")
        .bind(*run.root_frame_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("error");
    assert_eq!(error["code"], "tool_contract_changed");
}

#[tokio::test]
async fn failed_tool_invocation_becomes_a_durable_tool_failed_output() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("tool-failure-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
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
    let registry = Arc::new(InProcessToolRegistry::new());
    registry
        .register(bind_tool(Arc::new(FailingTool {
            descriptor: descriptor.clone(),
        })))
        .unwrap();
    let catalog = registry
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
    let name = format!("toolfailure{}", Uuid::now_v7().simple());
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
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([
            vec![
                json!({"type":"function_call","call_id":"failed-call","name":"docs/search","arguments":{}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"complete-after-failure","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"handled"}}}),
            ],
        ])))),
        tools: registry,
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    engine.tick_run(run.id.clone()).await.unwrap();
    let error: Value = sqlx::query("SELECT error FROM tool_calls WHERE frame_id = $1")
        .bind(*run.root_frame_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("error");
    assert_eq!(error["code"], "tool_failed");
    engine.tick_run(run.id.clone()).await.unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("state");
    assert_eq!(state, "completed");
}

#[tokio::test]
async fn allow_once_resolves_and_invokes_the_pending_durable_call() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("permission-{}", Uuid::now_v7())).unwrap(),
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
    let registry = Arc::new(InProcessToolRegistry::new());
    registry
        .register(bind_tool(Arc::new(SearchTool {
            descriptor: descriptor.clone(),
        })))
        .unwrap();
    let catalog = registry
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
    let name = format!("p{}", Uuid::now_v7().simple());
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
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([
            vec![
                json!({"type":"function_call","call_id":"call-1","name":"docs/search","arguments":{}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"complete-1","name":"complete_task","arguments":{"outcome":"ok","fields":{"answer":"done"}}}),
            ],
        ])))),
        tools: registry,
        authorizer: Arc::new(RequirePermission),
        config: EngineConfig::default(),
    };
    engine.tick_run(run.id.clone()).await.unwrap();
    let request_id: Uuid = sqlx::query("SELECT id FROM permission_requests WHERE run_id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("id");
    let approval_key = IdempotencyKey::new(format!("approval-{}", Uuid::now_v7())).unwrap();
    let approval = ResolvePermission {
        session_id: session.id.clone(),
        run_id: run.id.clone(),
        permission_request_id: kiteframe::PermissionRequestId::new(request_id),
        choice: PermissionChoice::AllowOnce,
        idempotency_key: approval_key.clone(),
    };
    engine.resolve_permission(approval.clone()).await.unwrap();
    engine.resolve_permission(approval).await.unwrap();
    let conflict = engine
        .resolve_permission(ResolvePermission {
            session_id: session.id.clone(),
            run_id: run.id.clone(),
            permission_request_id: kiteframe::PermissionRequestId::new(request_id),
            choice: PermissionChoice::DenyToolForSession,
            idempotency_key: approval_key,
        })
        .await
        .expect_err("same key with a conflicting permission choice must be rejected");
    assert!(matches!(
        conflict,
        kiteframe::EngineError::Store(kiteframe::StoreError::IdempotencyConflict)
    ));
    assert_eq!(
        store
            .policy_revision(session.id.clone())
            .await
            .unwrap()
            .get(),
        0
    );
    engine.tick_run(run.id.clone()).await.unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("state");
    assert_eq!(state, "completed");
}

#[tokio::test]
async fn conflicting_session_permission_choices_commit_one_matching_policy_atomically() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("permission-race-{}", Uuid::now_v7())).unwrap(),
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
    let registry = Arc::new(InProcessToolRegistry::new());
    registry
        .register(bind_tool(Arc::new(SearchTool {
            descriptor: descriptor.clone(),
        })))
        .unwrap();
    let catalog = registry
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
    let name = format!("pr{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok(answer: string) {{ done answer }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).unwrap();
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .unwrap();
    let workflow_name = definition.name.clone();
    let run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    let second_run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([
            vec![
                json!({"type":"function_call","call_id":"race-call-a","name":"docs/search","arguments":{}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"race-call-b","name":"docs/search","arguments":{}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"close-race-call","name":"docs/search","arguments":{}}),
            ],
        ])))),
        tools: registry,
        authorizer: Arc::new(RequirePermission),
        config: EngineConfig::default(),
    };
    let (first_tick, second_tick) = tokio::join!(
        engine.tick_run(run.id.clone()),
        engine.tick_run(second_run.id.clone())
    );
    first_tick.unwrap();
    second_tick.unwrap();
    let request_id: Uuid =
        sqlx::query_scalar("SELECT id FROM permission_requests WHERE run_id = $1")
            .bind(*run.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
    let second_request_id: Uuid =
        sqlx::query_scalar("SELECT id FROM permission_requests WHERE run_id = $1")
            .bind(*second_run.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
    let key = IdempotencyKey::new(format!("permission-race-{}", Uuid::now_v7())).unwrap();
    let allow = ResolvePermission {
        session_id: session.id.clone(),
        run_id: run.id.clone(),
        permission_request_id: kiteframe::PermissionRequestId::new(request_id),
        choice: PermissionChoice::AllowToolForSession,
        idempotency_key: key.clone(),
    };
    let deny = ResolvePermission {
        session_id: session.id.clone(),
        run_id: second_run.id.clone(),
        permission_request_id: kiteframe::PermissionRequestId::new(second_request_id),
        choice: PermissionChoice::DenyToolForSession,
        idempotency_key: key,
    };
    let (allow_result, deny_result) = tokio::join!(
        engine.resolve_permission(allow),
        engine.resolve_permission(deny)
    );
    let committed = allow_result.is_ok() as usize + deny_result.is_ok() as usize;
    assert!(
        committed >= 1,
        "at least one concurrent decision must commit"
    );
    for failure in [allow_result.as_ref().err(), deny_result.as_ref().err()]
        .into_iter()
        .flatten()
    {
        assert!(
            matches!(
                failure,
                kiteframe::EngineError::Store(kiteframe::StoreError::PolicyRevisionConflict)
            ),
            "unexpected losing decision result: {failure:?}"
        );
    }
    let policy = store
        .current_session_policy(session.id.clone())
        .await
        .unwrap();
    assert_eq!(
        policy.rules.len(),
        committed,
        "no committed policy rule may be overwritten"
    );
    for (result, permission_id, expected_effect) in [
        (&allow_result, request_id, kiteframe::PolicyEffect::Permit),
        (
            &deny_result,
            second_request_id,
            kiteframe::PolicyEffect::Forbid,
        ),
    ] {
        if result.is_ok() {
            assert!(
                policy
                    .rules
                    .iter()
                    .any(|rule| rule.rule_id == format!("permission-{permission_id}")
                        && rule.effect == expected_effect)
            );
        }
    }
    assert_eq!(
        store
            .policy_revision(session.id.clone())
            .await
            .unwrap()
            .get(),
        committed as u64
    );

    let close_race_run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("run-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    engine.tick_run(close_race_run.id.clone()).await.unwrap();
    let close_race_request: Uuid =
        sqlx::query_scalar("SELECT id FROM permission_requests WHERE run_id = $1")
            .bind(*close_race_run.id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
    let resolution = engine.resolve_permission(ResolvePermission {
        session_id: session.id.clone(),
        run_id: close_race_run.id,
        permission_request_id: kiteframe::PermissionRequestId::new(close_race_request),
        choice: PermissionChoice::AllowToolForSession,
        idempotency_key: IdempotencyKey::new(format!("close-race-{}", Uuid::now_v7())).unwrap(),
    });
    let closing = store.close_session_and_cancel_work(session.id.clone(), &session.principal);
    let (resolution, closing) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(resolution, closing)
    })
    .await
    .expect("session close and permission response must not deadlock");
    closing.unwrap();
    assert!(
        resolution.is_ok()
            || matches!(
                resolution,
                Err(kiteframe::EngineError::Store(
                    kiteframe::StoreError::SessionClosed | kiteframe::StoreError::RevisionConflict
                ))
            ),
        "unexpected concurrent resolution result: {resolution:?}"
    );
}

#[tokio::test]
async fn scoped_answer_resumes_a_durable_question() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.expect("store"));
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("question-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let registry = Arc::new(InProcessToolRegistry::new());
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("q{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ask_one(question: string) {{ answer = ask string question done answer }} }}"
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
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([vec![
            json!({"type":"function_call","call_id":"complete-1","name":"complete_task","arguments":{"outcome":"ask_one","fields":{"question":"Continue?"}}}),
        ]])))),
        tools: registry,
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    engine.tick_run(run.id.clone()).await.unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    let question_id: Uuid = sqlx::query("SELECT id FROM questions WHERE run_id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("id");
    engine
        .answer_question(AnswerQuestion {
            session_id: session.id.clone(),
            run_id: run.id.clone(),
            question_id,
            answer: json!("yes"),
            idempotency_key: IdempotencyKey::new(format!("answer-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    engine.tick_run(run.id.clone()).await.unwrap();
    let state: String = sqlx::query("SELECT state FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("state");
    assert_eq!(state, "completed");
}
