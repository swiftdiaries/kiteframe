use std::collections::BTreeSet;
use std::sync::Arc;

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, IdempotencyKey, PostgresStore,
    PrincipalId, RecoverySupport, StartRun, ToolCatalogSnapshot,
};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn restart_reconstructs_principal_scoped_run_and_cursor_replay() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let first = Arc::new(
        PostgresStore::connect(&database_url)
            .await
            .expect("first store"),
    );
    let principal = PrincipalId::new(format!("restart-a-{}", Uuid::now_v7())).unwrap();
    let session = first
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/restart-a".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let other_principal = PrincipalId::new(format!("restart-b-{}", Uuid::now_v7())).unwrap();
    let other_session = first
        .create_session(CreateSession {
            principal: other_principal.clone(),
            working_directory: "/restart-b".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("other session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [tool("docs/search")])
            .expect("catalog");
    first
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let source = format!(
        "workflow restart{}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok() {{ done \"ok\" }} }}",
        Uuid::now_v7().simple()
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("compile");
    let definition = first
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let run = first
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("restart-run-{}", Uuid::now_v7()))
                .unwrap(),
        })
        .await
        .expect("start");
    drop(first);

    let restarted = Arc::new(
        PostgresStore::connect(&database_url)
            .await
            .expect("restarted store"),
    );
    assert_eq!(
        restarted
            .resume_session(session.id.clone(), &principal)
            .await
            .expect("authenticated resume")
            .id,
        session.id
    );
    assert!(
        restarted
            .resume_session(session.id.clone(), &other_principal)
            .await
            .is_err()
    );
    assert!(
        restarted
            .get_run(other_session.id, run.id.clone())
            .await
            .is_err()
    );
    assert_eq!(
        restarted
            .get_run(session.id.clone(), run.id.clone())
            .await
            .expect("durable run")
            .root_frame_id,
        run.root_frame_id
    );
    let events = restarted
        .events_after(session.id.clone(), run.id, 0, 8)
        .await
        .expect("cursor replay");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "run.started");
}

fn tool(name: &str) -> CatalogTool {
    CatalogTool::new(
        name,
        "docs",
        "search",
        ClosedSchema::object("input", Default::default()),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("tool")
}
