use std::collections::BTreeSet;

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, IdempotencyKey, PostgresStore,
    PrincipalId, RecoverySupport, StartRun, StoreError, ToolCatalogSnapshot,
};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn start_run_snapshots_definition_and_replays_only_identical_requests() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping execution store test: DATABASE_URL is not configured");
        return;
    };
    let store = PostgresStore::connect(&database_url).await.expect("connect store");
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("alice-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("create session");
    let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [tool("docs/search")])
        .expect("catalog");
    store.append_catalog_epoch(&catalog).await.expect("append catalog");

    let workflow_name = format!("w{}", Uuid::now_v7().simple());
    let source = format!("workflow {workflow_name}(query: string) -> string {{ start t(query: query) }} task t(query: string) -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok() {{ done query }} }}");
    let workflow = compile(&source, &catalog, &Limits::default()).expect("compile workflow");
    let definition = store.publish(session.id.clone(), &workflow, &source).await.expect("publish");
    let key = IdempotencyKey::new(format!("start-{}", Uuid::now_v7())).unwrap();
    let request = StartRun {
        session_id: session.id.clone(),
        workflow_name: definition.name.clone(),
        version: Some(definition.version),
        input: json!({"query": "durable"}),
        idempotency_key: key.clone(),
    };

    let started = store.start_run(request.clone()).await.expect("start run");
    assert_eq!(started.definition, definition);
    assert_eq!(started.session_id, session.id);
    assert_eq!(started.state, kiteframe::RunState::Runnable);
    assert_eq!(started.revision, 0);
    assert_eq!(store.start_run(request.clone()).await.expect("replay"), started);
    let attempt = store
        .begin_model_attempt(started.root_frame_id.clone(), 0, json!({"prompt": "x"}))
        .await
        .expect("persist model intent");
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.revision, 1);
    store
        .commit_model_attempt(
            started.root_frame_id.clone(),
            attempt.attempt,
            attempt.revision,
            vec![json!({"type": "reasoning"}), json!({"type": "tool_call"})],
        )
        .await
        .expect("persist ordered model result");
    assert!(matches!(
        store
            .start_run(StartRun { input: json!({"query": "changed"}), ..StartRun {
                session_id: session.id,
                workflow_name: definition.name,
                version: Some(definition.version),
                input: json!({"query": "ignored"}),
                idempotency_key: key,
            } })
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
}

fn tool(name: &str) -> CatalogTool {
    let alias = name.split_once('/').expect("qualified name").0;
    CatalogTool::new(
        name,
        alias,
        "test tool",
        ClosedSchema::object("input", Default::default()),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("tool")
}
