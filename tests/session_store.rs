use std::collections::BTreeSet;

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, PolicyRevision, PolicyUpdate,
    PostgresStore, PrincipalId, RecoverySupport, StoreError, ToolCatalogSnapshot,
};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn session_authority_enforces_identity_catalog_policy_and_publication_idempotency() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping session store test: DATABASE_URL is not configured");
        return;
    };
    let store = PostgresStore::connect(&database_url)
        .await
        .expect("connect store");
    let principal = PrincipalId::new(format!("alice-{}", Uuid::now_v7())).expect("principal");
    let session = store
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("create session");
    assert!(matches!(
        store
            .resume_session(session.id.clone(), &PrincipalId::new("bob").unwrap())
            .await,
        Err(StoreError::PrincipalMismatch)
    ));

    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [tool("docs/search")])
            .expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("append catalog");
    assert!(
        store
            .resume_session(session.id.clone(), &principal)
            .await
            .expect("resume")
            .mcp_attached
    );

    let key = format!("policy-{}", Uuid::now_v7());
    let update = || PolicyUpdate {
        session: session.id.clone(),
        expected_revision: PolicyRevision::new(0),
        idempotency_key: key.clone(),
        typed_source: json!({"version": 1, "rules": []}),
        compiled_cedar: "permit(principal, action, resource);".to_owned(),
    };
    assert_eq!(
        store
            .append_policy_rules(update())
            .await
            .expect("policy update")
            .get(),
        1
    );
    assert_eq!(
        store
            .append_policy_rules(update())
            .await
            .expect("policy replay")
            .get(),
        1
    );
    assert!(matches!(
        store
            .append_policy_rules(PolicyUpdate {
                expected_revision: PolicyRevision::new(0),
                idempotency_key: format!("policy-{}", Uuid::now_v7()),
                ..update()
            })
            .await,
        Err(StoreError::PolicyRevisionConflict)
    ));

    let source = "workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [search] on ok() { done \"x\" } }";
    let workflow = compile(source, &catalog, &Limits::default()).expect("compile workflow");
    let first = store
        .publish(session.id.clone(), &workflow, source)
        .await
        .expect("publish");
    let replay = store
        .publish(session.id.clone(), &workflow, source)
        .await
        .expect("replay publish");
    assert_eq!(first, replay);

    let other = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("eve-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/other".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("other session");
    assert!(matches!(
        store.publish(other.id, &workflow, source).await,
        Err(StoreError::CatalogScopeViolation)
    ));
    store.close_session(session.id.clone(), &principal).await.expect("close session");
    assert!(matches!(store.resume_session(session.id, &principal).await, Err(StoreError::SessionClosed)));
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
