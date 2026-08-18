use std::borrow::Cow;

use kiteframe::dsl::{CompiledDefinition, DefinitionKind, Limits, compile};
use kiteframe::{CreateSession, IdempotencyKey, PostgresStore, PrincipalId, StartRun, StoreError};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

fn compile_definition(source: &str) -> CompiledDefinition {
    let catalog = kiteframe::ToolCatalogSnapshot::new(
        kiteframe::SessionId::new(Uuid::nil()),
        Uuid::from_u128(7),
        [],
    )
    .unwrap();
    compile(source, &catalog, &Limits::default()).expect("definition compiles")
}

fn compiled_babysit_agent() -> CompiledDefinition {
    compile_definition(
        r#"agent babysit_pr(repository: string) -> string {
  entry inspect(repository = repository)
  events ["github.pull_request.changed.v1"]
  limits { steps: 20 active_execution_seconds: 30 elapsed_seconds: 60 }
  task inspect(repository: string) -> string {
    model reasoning
    prompt """Inspect the pull request."""
    tools []
    on done(summary: string) { done summary }
  }
}"#,
    )
}

fn compiled_support_workflow() -> CompiledDefinition {
    compile_definition(
        r#"workflow support(ticket: string) -> string {
  start triage(ticket: ticket)
  task triage(ticket: string) -> string {
    prompt """Triage the support request."""
    tools []
    on done(answer: string) { done answer }
  }
}"#,
    )
}

async fn migrated_store_through_0011() -> Option<(PostgresStore, sqlx::PgPool)> {
    let database_url = std::env::var("DATABASE_URL").ok()?;
    let mut admin_url = url::Url::parse(&database_url).expect("database URL");
    admin_url.set_path("/postgres");
    let database_name = format!("kiteframe_definition_{}", Uuid::now_v7().simple());
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url.as_str())
        .await
        .expect("connect database administrator");
    sqlx::query(&format!("CREATE DATABASE \"{database_name}\""))
        .execute(&admin)
        .await
        .expect("create isolated definition database");
    admin.close().await;

    let mut database_url = admin_url;
    database_url.set_path(&format!("/{database_name}"));
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url.as_str())
        .await
        .expect("connect isolated definition database");
    let through_0011 = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 11)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    through_0011.run(&pool).await.expect("migrate through 0011");
    sqlx::query(
        "INSERT INTO workflow_definitions \
         (id, name, version, definition_hash, source, compiled_definition, tool_bindings) \
         VALUES ($1, 'historical', 7, 'sha256:historical', 'historical', $2, '[]'::jsonb)",
    )
    .bind(Uuid::now_v7())
    .bind(json!({"dsl_version": 1, "name": "historical"}))
    .execute(&pool)
    .await
    .expect("seed historical definition");

    Some((PostgresStore::from_migrated_pool(pool.clone()), pool))
}

#[tokio::test]
async fn migrated_definitions_are_tagged_without_changing_versions() {
    let Some((store, pool)) = migrated_store_through_0011().await else {
        eprintln!("skipping definition migration test: DATABASE_URL is not configured");
        return;
    };
    MIGRATOR.run(&pool).await.unwrap();

    let old_rows: Vec<(String, i32, String)> = sqlx::query_as(
        "SELECT name, version, definition_kind FROM workflow_definitions \
         WHERE name = 'historical'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        old_rows,
        vec![("historical".to_owned(), 7, "workflow".to_owned())]
    );

    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("migration-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/migration".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await;
    let session = session.unwrap();
    let workflow = store
        .publish(
            session.id.clone(),
            &compiled_support_workflow(),
            "historical migration workflow",
        )
        .await
        .unwrap();
    let agent = store
        .publish(
            session.id,
            &compiled_babysit_agent(),
            "historical migration agent",
        )
        .await
        .unwrap();
    assert_eq!(workflow.kind, DefinitionKind::Workflow);
    assert_eq!(agent.kind, DefinitionKind::Agent);

    let database_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    drop(store);
    pool.close().await;
    let mut admin_url = url::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
    admin_url.set_path("/postgres");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url.as_str())
        .await
        .unwrap();
    sqlx::query(&format!("DROP DATABASE \"{database_name}\" WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
async fn publication_preserves_definition_kind_and_global_name_versions() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping definition store test: DATABASE_URL is not configured");
        return;
    };
    let store = PostgresStore::connect(&database_url).await.unwrap();
    let principal = PrincipalId::new(format!("definition-{}", Uuid::now_v7())).unwrap();
    let session = store
        .create_session(CreateSession {
            principal,
            working_directory: "/definitions".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let shared_name = format!("shared_{}", Uuid::now_v7().simple());
    let workflow = compile_definition(&format!(
        "workflow {shared_name}() -> string {{ start t() }} \
         task t() -> string {{ prompt \"\"\"one\"\"\" tools [] on done() {{ done \"one\" }} }}"
    ));
    let agent = compile_definition(&format!(
        "agent {shared_name}() -> string {{ entry t() \
         events [\"test.changed.v1\"] \
         limits {{ steps: 5 active_execution_seconds: 10 elapsed_seconds: 20 }} \
         task t() -> string {{ model reasoning prompt \"\"\"two\"\"\" tools [] \
         on done() {{ done \"two\" }} }} }}"
    ));

    let published_workflow = store
        .publish(session.id.clone(), &workflow, &workflow.source)
        .await
        .unwrap();
    let published_agent = store
        .publish(session.id.clone(), &agent, &agent.source)
        .await
        .unwrap();
    let replay = store
        .publish(session.id.clone(), &agent, &agent.source)
        .await
        .unwrap();

    assert_eq!(published_workflow.kind, DefinitionKind::Workflow);
    assert_eq!(published_agent.kind, DefinitionKind::Agent);
    assert_eq!(published_agent.version, published_workflow.version + 1);
    assert_eq!(published_agent, replay);

    assert!(matches!(
        store
            .start_run(StartRun {
                session_id: session.id.clone(),
                workflow_name: shared_name,
                version: Some(published_agent.version),
                input: json!({}),
                idempotency_key: IdempotencyKey::new("agent-cannot-start").unwrap(),
            })
            .await,
        Err(StoreError::AgentRuntimeUnavailable),
    ));
    let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE session_id = $1")
        .bind(session.id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(run_count, 0);
}
