use std::sync::Arc;

use kiteframe::dsl::{CompiledDefinition, DefinitionKind, Limits, compile};
use kiteframe::{
    AgentSelector, CreateSession, GoalContract, HarnessRole, IdempotencyKey, PostgresStore,
    PrincipalId, PublishedDefinition, SessionId, SessionView, StoreError, SubmitUserTask,
    SuccessCriterion, TaskPreferences, TaskTarget, UserTaskId,
};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tokio::sync::Barrier;
use uuid::Uuid;

fn compile_definition(source: &str) -> CompiledDefinition {
    let catalog =
        kiteframe::ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(7), [])
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

fn changed_babysit_agent() -> CompiledDefinition {
    compile_definition(
        r#"agent babysit_pr(repository: string) -> string {
  entry inspect(repository = repository)
  events ["github.pull_request.changed.v1"]
  limits { steps: 21 active_execution_seconds: 30 elapsed_seconds: 60 }
  task inspect(repository: string) -> string {
    model reasoning
    prompt """Inspect the pull request with refreshed evidence."""
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

async fn setup_agent_store() -> Option<(
    Arc<PostgresStore>,
    PgPool,
    SessionView,
    PrincipalId,
    PublishedDefinition,
)> {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping task store test: DATABASE_URL is not configured");
        return None;
    };
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let principal = PrincipalId::new(format!("task-{}", Uuid::now_v7())).unwrap();
    let session = store
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/tasks".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let agent = compiled_babysit_agent();
    let published = store
        .publish(session.id.clone(), &agent, &agent.source)
        .await
        .unwrap();
    Some((
        store.clone(),
        store.pool().clone(),
        session,
        principal,
        published,
    ))
}

async fn task_count(pool: &PgPool, session_id: &SessionId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM user_tasks WHERE session_id = $1")
        .bind(session_id.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn goal_count(pool: &PgPool, session_id: &SessionId) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM goal_contracts g \
         JOIN user_tasks t ON t.id = g.task_id WHERE t.session_id = $1",
    )
    .bind(session_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

fn submit_request(
    session_id: SessionId,
    agent_name: String,
    version: Option<u32>,
) -> SubmitUserTask {
    SubmitUserTask {
        session_id,
        agent: AgentSelector {
            name: agent_name,
            version,
        },
        target: TaskTarget::GithubPullRequest {
            repository: "acme/widgets".to_owned(),
            number: 142,
        },
        preferences: TaskPreferences { harness_role: None },
        goal: GoalContract {
            version: 1,
            objective: "Keep the pull request mergeable".to_owned(),
            success: vec![SuccessCriterion {
                id: "reviews_resolved".to_owned(),
                statement: "All actionable reviews are resolved".to_owned(),
                evidence: "github.review_thread".to_owned(),
            }],
            constraints: vec!["Do not rewrite published history".to_owned()],
            blocked_when: vec!["Required credentials are unavailable".to_owned()],
        },
        idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7())).unwrap(),
    }
}

#[test]
fn slice_one_schema_tags_definitions_and_freezes_goals() {
    let migration = std::fs::read_to_string("migrations/0012_user_tasks_and_goal_contracts.sql")
        .expect("foundation migration exists");

    assert!(migration.contains("ADD COLUMN definition_kind"));
    assert!(migration.contains("DEFAULT 'workflow'"));
    assert!(migration.contains("CREATE TABLE user_tasks"));
    assert!(migration.contains("CREATE TABLE goal_contracts"));
    assert!(migration.contains("goal_contracts_reject_update"));
    assert!(migration.contains("goal_contracts_reject_delete"));
    assert!(migration.contains("ERRCODE = '55000'"));
}

#[tokio::test]
async fn submission_atomically_freezes_agent_and_goal() {
    let Some((store, _pool, session, principal, agent)) = setup_agent_store().await else {
        eprintln!("skipping task store test: DATABASE_URL is not configured");
        return;
    };
    let request = submit_request(session.id.clone(), agent.name.clone(), None);
    let accepted = store
        .submit_user_task(&principal, request.clone())
        .await
        .unwrap();

    assert_eq!(accepted.definition.id, agent.id);
    assert_eq!(accepted.definition.kind, DefinitionKind::Agent);
    assert_eq!(accepted.revision, 0);
    assert_eq!(accepted.goal_digest, request.goal.digest().unwrap());
    assert_eq!(
        store
            .get_user_task(session.id, accepted.id.clone())
            .await
            .unwrap(),
        accepted,
    );
}

#[tokio::test]
async fn exact_version_is_respected() {
    let Some((store, _pool, session, principal, version_one)) = setup_agent_store().await else {
        return;
    };
    let changed = changed_babysit_agent();
    let version_two = store
        .publish(session.id.clone(), &changed, &changed.source)
        .await
        .unwrap();
    assert!(version_two.version > version_one.version);

    let accepted = store
        .submit_user_task(
            &principal,
            submit_request(
                session.id,
                version_one.name.clone(),
                Some(version_one.version),
            ),
        )
        .await
        .unwrap();
    assert_eq!(accepted.definition.id, version_one.id);
}

#[tokio::test]
async fn replay_keeps_the_original_version_while_a_fresh_key_selects_latest() {
    let Some((store, _pool, session, principal, version_one)) = setup_agent_store().await else {
        return;
    };
    let original_request = submit_request(session.id.clone(), version_one.name.clone(), None);
    let original = store
        .submit_user_task(&principal, original_request.clone())
        .await
        .unwrap();
    let changed = changed_babysit_agent();
    let version_two = store
        .publish(session.id.clone(), &changed, &changed.source)
        .await
        .unwrap();

    let replay = store
        .submit_user_task(&principal, original_request)
        .await
        .unwrap();
    assert_eq!(replay.definition.version, original.definition.version);
    let mut fresh = submit_request(session.id, version_one.name, None);
    fresh.idempotency_key = IdempotencyKey::new("fresh-version-selection").unwrap();
    let latest = store.submit_user_task(&principal, fresh).await.unwrap();
    assert_eq!(latest.definition.version, version_two.version);
    assert_eq!(original.id, replay.id);
}

#[tokio::test]
async fn concurrent_replay_creates_one_task_and_one_goal() {
    let Some((store, pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let request = submit_request(session.id.clone(), agent.name, None);
    let barrier = Arc::new(Barrier::new(3));
    let submit = |store: Arc<PostgresStore>, barrier: Arc<Barrier>| {
        let principal = principal.clone();
        let request = request.clone();
        async move {
            barrier.wait().await;
            store.submit_user_task(&principal, request).await.unwrap()
        }
    };
    let first = tokio::spawn(submit(store.clone(), barrier.clone()));
    let second = tokio::spawn(submit(store, barrier.clone()));
    barrier.wait().await;
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.unwrap().id, second.unwrap().id);
    assert_eq!(task_count(&pool, &session.id).await, 1);
    assert_eq!(goal_count(&pool, &session.id).await, 1);
}

#[tokio::test]
async fn same_key_with_changed_semantics_conflicts_before_definition_resolution() {
    let Some((store, pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let base = submit_request(session.id.clone(), agent.name, None);
    let accepted = store
        .submit_user_task(&principal, base.clone())
        .await
        .unwrap();
    let mut changed = Vec::new();

    let mut request = base.clone();
    request.target = TaskTarget::GithubPullRequest {
        repository: "acme/other".into(),
        number: 142,
    };
    changed.push(request);
    let mut request = base.clone();
    request.preferences.harness_role = Some(HarnessRole::Reasoning);
    changed.push(request);
    let mut request = base.clone();
    request.agent.version = Some(agent.version + 99);
    changed.push(request);
    let mut request = base;
    request.goal.objective = "Changed objective".into();
    changed.push(request);

    for request in changed {
        assert!(matches!(
            store.submit_user_task(&principal, request).await,
            Err(StoreError::IdempotencyConflict),
        ));
    }
    assert_eq!(task_count(&pool, &accepted.session_id).await, 1);
    assert_eq!(goal_count(&pool, &accepted.session_id).await, 1);
}

#[tokio::test]
async fn rejected_submissions_leave_no_partial_rows() {
    let Some((store, pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let workflow = compiled_support_workflow();
    let workflow = store
        .publish(session.id.clone(), &workflow, &workflow.source)
        .await
        .unwrap();

    assert!(matches!(
        store
            .submit_user_task(
                &principal,
                submit_request(session.id.clone(), workflow.name, None),
            )
            .await,
        Err(StoreError::AgentDefinitionRequired),
    ));
    assert!(matches!(
        store
            .submit_user_task(
                &principal,
                submit_request(session.id.clone(), "missing_agent".to_owned(), None),
            )
            .await,
        Err(StoreError::DefinitionNotFound),
    ));
    let wrong_principal = PrincipalId::new("wrong-principal").unwrap();
    assert!(matches!(
        store
            .submit_user_task(
                &wrong_principal,
                submit_request(session.id.clone(), agent.name.clone(), None),
            )
            .await,
        Err(StoreError::PrincipalMismatch),
    ));
    let mut invalid = submit_request(session.id.clone(), agent.name.clone(), None);
    invalid.target = TaskTarget::GithubPullRequest {
        repository: "acme/widgets".to_owned(),
        number: 0,
    };
    assert!(matches!(
        store.submit_user_task(&principal, invalid).await,
        Err(StoreError::InvalidUserTask(_)),
    ));
    store
        .close_session(session.id.clone(), &principal)
        .await
        .unwrap();
    assert!(matches!(
        store
            .submit_user_task(
                &principal,
                submit_request(session.id.clone(), agent.name, None),
            )
            .await,
        Err(StoreError::SessionClosed),
    ));
    assert_eq!(task_count(&pool, &session.id).await, 0);
    assert_eq!(goal_count(&pool, &session.id).await, 0);
}

#[tokio::test]
async fn goal_round_trip_preserves_bytes_and_database_rejects_mutation() {
    let Some((store, pool, session, principal, agent)) = setup_agent_store().await else {
        return;
    };
    let mut request = submit_request(session.id, agent.name, None);
    request.goal.objective = "  Preserve these exact bytes  ".to_owned();
    request.goal.success[0].statement = "Keep  internal  spacing".to_owned();
    let accepted = store
        .submit_user_task(&principal, request.clone())
        .await
        .unwrap();
    assert_eq!(accepted.goal, request.goal);
    assert_eq!(accepted.goal_digest, accepted.goal.digest().unwrap());

    for statement in [
        "UPDATE goal_contracts SET objective = 'changed' WHERE task_id = $1",
        "DELETE FROM goal_contracts WHERE task_id = $1",
    ] {
        let error = sqlx::query(statement)
            .bind(accepted.id.as_uuid())
            .execute(&pool)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("55000")
        );
    }
    assert_eq!(
        store
            .get_user_task(accepted.session_id, accepted.id)
            .await
            .unwrap()
            .goal,
        accepted.goal
    );
}

#[tokio::test]
async fn missing_task_is_typed() {
    let Some((store, _pool, session, _principal, _agent)) = setup_agent_store().await else {
        return;
    };
    assert!(matches!(
        store
            .get_user_task(session.id, UserTaskId::new(Uuid::now_v7()))
            .await,
        Err(StoreError::UserTaskNotFound),
    ));
}
