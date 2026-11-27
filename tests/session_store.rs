use std::collections::BTreeSet;
use std::sync::Arc;

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, PersistedAuthorizer, PolicyDecision,
    PolicyEffect, PolicyRequest, PolicyRevision, PolicyRule, PolicyTarget, PolicyUpdate,
    PostgresStore, PrincipalId, RecoverySupport, SessionAuthorizer, SessionPolicy, StoreError,
    StoreLimits, ToolCatalogSnapshot,
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
    let store = Arc::new(
        PostgresStore::connect(&database_url)
            .await
            .expect("connect store"),
    );
    let principal = PrincipalId::new(format!("alice-{}", Uuid::now_v7())).expect("principal");
    let session = store
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("create session");
    assert!(
        !session.mcp_attached,
        "the initial empty catalog is not an external MCP attachment"
    );
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
    let restored = store
        .current_catalog(session.id.clone())
        .await
        .expect("restore catalog");
    assert_eq!(restored.digest(), catalog.digest());
    assert_eq!(
        restored
            .resolve(
                "docs/search",
                catalog.tools()["docs/search"].descriptor_digest()
            )
            .expect("restored descriptor")
            .descriptor_digest(),
        catalog.tools()["docs/search"].descriptor_digest()
    );
    assert!(
        !store
            .resume_session(session.id.clone(), &principal)
            .await
            .expect("resume")
            .mcp_attached,
        "a durable local catalog does not require an MCP reconnect"
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
    let permit = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "permit-search".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["docs/search".to_owned()]),
            },
            input: Default::default(),
        }],
    };
    store
        .append_session_policy(
            session.id.clone(),
            PolicyRevision::new(1),
            "permit-search".to_owned(),
            &permit,
            &catalog,
        )
        .await
        .expect("append permit");
    let authorizer = PersistedAuthorizer::new(store.clone());
    let request = PolicyRequest {
        session: session.id.clone(),
        qualified_name: "docs/search".to_owned(),
        descriptor_digest: catalog.tools()["docs/search"]
            .descriptor_digest()
            .to_owned(),
        capabilities: vec![],
        arguments: serde_json::json!({}),
    };
    assert!(matches!(
        authorizer.authorize(&request).await,
        Ok(PolicyDecision::Allow)
    ));
    let forbid = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "forbid-search".to_owned(),
            effect: PolicyEffect::Forbid,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["docs/search".to_owned()]),
            },
            input: Default::default(),
        }],
    };
    store
        .append_session_policy(
            session.id.clone(),
            PolicyRevision::new(2),
            "forbid-search".to_owned(),
            &forbid,
            &catalog,
        )
        .await
        .expect("append forbid");
    assert!(matches!(
        authorizer.authorize(&request).await,
        Ok(PolicyDecision::Forbid { .. })
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
    store
        .close_session(session.id.clone(), &principal)
        .await
        .expect("close session");
    assert!(matches!(
        store.resume_session(session.id, &principal).await,
        Err(StoreError::SessionClosed)
    ));
}

#[tokio::test]
async fn active_session_quota_is_principal_scoped_and_releases_on_close() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = PostgresStore::connect_with_limits(
        &database_url,
        StoreLimits {
            max_active_sessions_per_principal: 1,
            ..StoreLimits::default()
        },
    )
    .await
    .expect("quota store");
    let principal = PrincipalId::new(format!("quota-{}", Uuid::now_v7())).unwrap();
    let request = CreateSession {
        principal: principal.clone(),
        working_directory: "/quota".to_owned(),
        retention_until: OffsetDateTime::now_utc() + Duration::days(1),
    };
    let first = store
        .create_session(request.clone())
        .await
        .expect("first session");
    assert!(matches!(
        store.create_session(request.clone()).await,
        Err(StoreError::QuotaExceeded("active_sessions_per_principal"))
    ));
    store
        .close_session(first.id, &principal)
        .await
        .expect("close first");
    store
        .create_session(request)
        .await
        .expect("capacity released");
}

#[tokio::test]
async fn concurrent_active_session_quota_admits_exactly_one_creation() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = std::sync::Arc::new(
        PostgresStore::connect_with_limits(
            &database_url,
            StoreLimits {
                max_active_sessions_per_principal: 1,
                ..StoreLimits::default()
            },
        )
        .await
        .expect("quota store"),
    );
    let principal = PrincipalId::new(format!("quota-race-{}", Uuid::now_v7())).unwrap();
    let request = CreateSession {
        principal: principal.clone(),
        working_directory: "/quota".to_owned(),
        retention_until: OffsetDateTime::now_utc() + Duration::days(1),
    };
    let (first, second) = tokio::join!(
        store.create_session(request.clone()),
        store.create_session(request)
    );
    let created = match (first, second) {
        (Ok(created), Err(StoreError::QuotaExceeded("active_sessions_per_principal")))
        | (Err(StoreError::QuotaExceeded("active_sessions_per_principal")), Ok(created)) => created,
        results => panic!("expected one creation and one quota rejection, got {results:?}"),
    };
    store.close_session(created.id, &principal).await.unwrap();
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
