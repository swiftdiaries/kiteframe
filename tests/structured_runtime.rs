use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    AnswerQuestion, CreateSession, Engine, EngineConfig, IdempotencyKey, InProcessToolRegistry,
    ModelClient, ModelError, ModelRequest, ModelResponse, PolicyDecision, PolicyRequest,
    PostgresStore, PrincipalId, SessionAuthorizer, StartRun, ToolCatalogSnapshot,
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

struct RecordingModel {
    responses: Mutex<VecDeque<Vec<Value>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

struct SlowModel;

#[async_trait]
impl ModelClient for SlowModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        Ok(ModelResponse { items: vec![] })
    }
}

#[async_trait]
impl ModelClient for RecordingModel {
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests.lock().unwrap().push(request);
        Ok(ModelResponse {
            items: self
                .responses
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

#[tokio::test]
async fn response_item_ceiling_durably_fails_the_run() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("response-limit-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("responselimit{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ limits {{ response_item_bytes: 1 }} start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"ok\" }} }}"
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
            json!({"unexpected":"oversized"}),
        ]])))),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };

    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    let row = sqlx::query("SELECT state, failure FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "failed");
    assert_eq!(
        row.get::<Value, _>("failure")["code"],
        "response_item_limit_exceeded"
    );
}

#[tokio::test]
async fn active_execution_ceiling_times_out_model_io_and_releases_its_lease() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("active-limit-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("activelimit{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ limits {{ active_execution_seconds: 1 }} start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"ok\" }} }}"
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
        model: Arc::new(SlowModel),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };

    assert_eq!(engine.tick_run(run.id.clone()).await.unwrap(), 1);
    let row = sqlx::query("SELECT state, failure, active_execution_count, active_execution_started_at FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "failed");
    assert_eq!(
        row.get::<Value, _>("failure")["code"],
        "active_execution_limit_exceeded"
    );
    assert_eq!(row.get::<i32, _>("active_execution_count"), 0);
    assert!(
        row.get::<Option<time::OffsetDateTime>, _>("active_execution_started_at")
            .is_none()
    );
}

#[tokio::test]
async fn parallel_children_complete_before_the_parent_resumes() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("parallel-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("parallel{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start root() }} \
         task root() -> string {{ prompt \"\"\"x\"\"\" tools [] on split() {{ parallel {{ left = call leaf(value: \"left\") right = call leaf(value: \"right\") }} done \"joined\" }} }} \
         task leaf(value: string) -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done value }} }}"
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
    let model = Arc::new(RecordingModel {
        responses: Mutex::new(VecDeque::from([
            vec![
                json!({"type":"function_call","call_id":"root","name":"complete_task","arguments":{"outcome":"split","fields":{}}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"left","name":"complete_task","arguments":{"outcome":"ok","fields":{}}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"right","name":"complete_task","arguments":{"outcome":"ok","fields":{}}}),
            ],
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let engine = Engine {
        store: store.clone(),
        model: model.clone(),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    for _ in 0..6 {
        engine.tick_run(run.id.clone()).await.unwrap();
    }
    let output: Option<Value> = sqlx::query("SELECT output FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("output");
    assert_eq!(output, Some(json!("joined")));
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for child in &requests[1..] {
        assert!(
            !child.input.iter().any(|item| item["call_id"] == "root"),
            "child transcript must not inherit parent model history"
        );
    }
}

#[tokio::test]
async fn parallel_questions_remain_independently_answerable_until_all_children_resume() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("parallel-questions-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("parallelquestions{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start root() }} \
         task root() -> string {{ prompt \"\"\"x\"\"\" tools [] on split() {{ parallel {{ left = call leaf(value: \"left\") right = call leaf(value: \"right\") }} done \"joined\" }} }} \
         task leaf(value: string) -> string {{ prompt \"\"\"x\"\"\" tools [] on ask_one(question: string) {{ answer = ask string question done answer }} }}"
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
                json!({"type":"function_call","call_id":"root","name":"complete_task","arguments":{"outcome":"split","fields":{}}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"left","name":"complete_task","arguments":{"outcome":"ask_one","fields":{"question":"left?"}}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"right","name":"complete_task","arguments":{"outcome":"ask_one","fields":{"question":"right?"}}}),
            ],
        ])))),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    for _ in 0..8 {
        engine.tick_run(run.id.clone()).await.unwrap();
    }
    let questions = sqlx::query("SELECT questions.id, questions.frame_id, frames.revision, questions.text, questions.state FROM questions JOIN frames ON frames.id = questions.frame_id WHERE questions.run_id = $1 ORDER BY questions.text")
        .bind(*run.id.as_uuid()).fetch_all(store.pool()).await.unwrap();
    assert_eq!(questions.len(), 2);
    let first_id: Uuid = questions[0].get("id");
    let second_id: Uuid = questions[1].get("id");
    engine
        .answer_question(AnswerQuestion {
            session_id: session.id.clone(),
            run_id: run.id.clone(),
            question_id: first_id,
            answer: json!("first answer"),
            idempotency_key: IdempotencyKey::new(format!("answer-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    let second_state: String = sqlx::query_scalar("SELECT state FROM questions WHERE id = $1")
        .bind(second_id)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(second_state, "pending");
    assert_ne!(
        store
            .get_run(session.id.clone(), run.id.clone())
            .await
            .unwrap()
            .state,
        kiteframe::RunState::Completed
    );
    engine
        .answer_question(AnswerQuestion {
            session_id: session.id.clone(),
            run_id: run.id.clone(),
            question_id: second_id,
            answer: json!("second answer"),
            idempotency_key: IdempotencyKey::new(format!("answer-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .unwrap();
    for _ in 0..6 {
        engine.tick_run(run.id.clone()).await.unwrap();
    }
    let output: Option<Value> = sqlx::query("SELECT output FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("output");
    assert_eq!(output, Some(json!("joined")));
}

#[tokio::test]
async fn durable_step_ceiling_fails_a_goto_loop_across_model_turns() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("step-limit-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let name = format!("steps{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ limits {{ steps: 1 }} start t() }} \
         task t() -> string {{ prompt \"\"\"x\"\"\" tools [] \
         on again() {{ goto t() }} on done(answer: string) {{ done answer }} }}"
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
                json!({"type":"function_call","call_id":"again","name":"complete_task","arguments":{"outcome":"again","fields":{}}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"done","name":"complete_task","arguments":{"outcome":"done","fields":{"answer":"never"}}}),
            ],
        ])))),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(AllowAll),
        config: EngineConfig::default(),
    };
    for _ in 0..4 {
        engine.tick_run(run.id.clone()).await.unwrap();
    }
    let failure: Value = sqlx::query("SELECT failure FROM runs WHERE id = $1")
        .bind(*run.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap()
        .get("failure");
    assert_eq!(failure["code"], "step_limit_exceeded");
}
