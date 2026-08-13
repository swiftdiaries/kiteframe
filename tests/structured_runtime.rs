use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CreateSession, Engine, EngineConfig, IdempotencyKey, InProcessToolRegistry, ModelClient,
    ModelError, ModelRequest, ModelResponse, PolicyDecision, PolicyRequest, PostgresStore,
    PrincipalId, SessionAuthorizer, StartRun, ToolCatalogSnapshot,
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
    let engine = Engine {
        store: store.clone(),
        model: Arc::new(ScriptedModel(Mutex::new(VecDeque::from([
            vec![
                json!({"type":"function_call","call_id":"root","name":"complete_task","arguments":{"outcome":"split","fields":{}}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"left","name":"complete_task","arguments":{"outcome":"ok","fields":{}}}),
            ],
            vec![
                json!({"type":"function_call","call_id":"right","name":"complete_task","arguments":{"outcome":"ok","fields":{}}}),
            ],
        ])))),
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
}
