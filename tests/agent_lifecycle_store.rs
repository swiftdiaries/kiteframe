use std::collections::BTreeSet;
use std::sync::Arc;

use kiteframe::dsl::{CompiledDefinition, Limits, compile};
use kiteframe::{
    AcceptedUserTask, AgentExecutionView, AgentSelector, AgentTaskEventKind, CatalogTool,
    ClosedSchema, CreateSession, EffectKind, GoalContract, GoalPhase, IdempotencyKey,
    OperationalStatus, PostgresStore, PrincipalId, RecoverySupport, RuntimeCeilings,
    RuntimeDisposition, RuntimeStateSnapshot, SessionView, StoreError, SubmitUserTask,
    SuccessCriterion, TaskLifecycle, TaskPreferences, TaskTarget, ToolCatalogSnapshot,
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct AgentStoreFixture {
    store: Arc<PostgresStore>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    task: AcceptedUserTask,
}

impl AgentStoreFixture {
    async fn accepted() -> Option<Self> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let principal = PrincipalId::new(format!("agent-store-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/agent-store".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let catalog = ToolCatalogSnapshot::new(
            session.id.clone(),
            Uuid::now_v7(),
            [CatalogTool::new(
                "docs/search",
                "docs",
                "Search documents.",
                ClosedSchema::object("input", Default::default()),
                ClosedSchema::object("output", Default::default()),
                BTreeSet::new(),
                EffectKind::ReadOnly,
                RecoverySupport::NotRequired,
            )
            .unwrap()],
        )
        .unwrap();
        store.append_catalog_epoch(&catalog).await.unwrap();
        let definition = compiled_agent(&catalog);
        let published = store
            .publish(session.id.clone(), &definition, &definition.source)
            .await
            .unwrap();
        let task = store
            .submit_user_task(
                &principal,
                SubmitUserTask {
                    session_id: session.id.clone(),
                    agent: AgentSelector {
                        name: published.name,
                        version: Some(published.version),
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
                    idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        Some(Self {
            pool: store.pool().clone(),
            store,
            session,
            principal,
            task,
        })
    }

    async fn with_history_rows() -> Option<Self> {
        let fixture = Self::accepted().await?;
        let snapshot = RuntimeStateSnapshot::new(
            RuntimeDisposition::Running,
            json!({"frames": []}),
            &RuntimeCeilings::default(),
        )
        .unwrap();
        sqlx::query(
            "UPDATE user_tasks SET state = 'running', operational_status = 'runnable', \
             revision = 1 WHERE id = $1",
        )
        .bind(fixture.task.id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_runtime_snapshots \
             (task_id, task_revision, schema_version, runtime_id, disposition, payload, \
              ceiling_bytes, canonical_bytes, snapshot_digest) \
             VALUES ($1, 1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(fixture.task.id.as_uuid())
        .bind(i32::from(snapshot.schema_version))
        .bind(&snapshot.runtime_id)
        .bind("running")
        .bind(&snapshot.payload)
        .bind(i32::try_from(snapshot.ceiling_bytes).unwrap())
        .bind(i32::try_from(snapshot.canonical_byte_count).unwrap())
        .bind(&snapshot.snapshot_digest)
        .execute(&fixture.pool)
        .await
        .unwrap();

        let transition_id = Uuid::now_v7();
        let command_id = Uuid::now_v7();
        let evidence_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO agent_transitions \
             (id, task_id, idempotency_key, request_digest, expected_task_revision, \
              expected_goal_revision, committed_task_revision, committed_goal_revision, \
              snapshot_digest, command_digests) \
             VALUES ($1, $2, 'start-1', 'sha256:request', 0, 0, 1, 0, $3, '[]'::jsonb)",
        )
        .bind(transition_id)
        .bind(fixture.task.id.as_uuid())
        .bind(&snapshot.snapshot_digest)
        .execute(&fixture.pool)
        .await
        .unwrap();
        let command_payload = json!({"evidenceType": "github.review_thread", "subject": {}});
        sqlx::query(
            "INSERT INTO agent_commands \
             (id, task_id, transition_id, ordinal, schema_version, runtime_key, \
              command_kind, payload, payload_digest) \
             VALUES ($1, $2, $3, 0, 1, 'context-1', 'request_context', $4, $5)",
        )
        .bind(command_id)
        .bind(fixture.task.id.as_uuid())
        .bind(transition_id)
        .bind(&command_payload)
        .bind(kiteframe::canonical_sha256(&command_payload).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_policy_decisions \
             (id, task_id, transition_id, action_kind, allowed, policy_revision, reason_code) \
             VALUES ($1, $2, $3, 'start', TRUE, 0, 'permitted')",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.task.id.as_uuid())
        .bind(transition_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO goal_evidence \
             (id, task_id, evidence_type, source_kind, source_identity, content_digest) \
             VALUES ($1, $2, 'github.review_thread', 'builtin_harness', $3, 'sha256:evidence')",
        )
        .bind(evidence_id)
        .bind(fixture.task.id.as_uuid())
        .bind(command_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO goal_criterion_assertions \
             (task_id, criterion_id, evidence_id, goal_revision) \
             VALUES ($1, 'reviews_resolved', $2, 1)",
        )
        .bind(fixture.task.id.as_uuid())
        .bind(evidence_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO goal_blockers \
             (id, task_id, blocked_when_index, reason_code, explanation, evidence_ids, \
              unlock_conditions, goal_revision) \
             VALUES ($1, $2, 0, 'credentials_missing', 'Credentials are unavailable', \
                     '[]'::jsonb, '[\"restore credentials\"]'::jsonb, 1)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.task.id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_task_actions \
             (id, task_id, action_kind, idempotency_key, request_digest, \
              result_task_revision, result_goal_revision) \
             VALUES ($1, $2, 'start', 'start-action-1', 'sha256:action', 1, 0)",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.task.id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_task_events \
             (task_id, sequence, kind, metadata, task_revision, goal_revision) \
             VALUES ($1, 1, 'agent.started', '{}'::jsonb, 1, 0)",
        )
        .bind(fixture.task.id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();
        Some(fixture)
    }

    async fn current(&self) -> AgentExecutionView {
        self.store
            .get_agent_execution(self.session.id.clone(), self.task.id.clone())
            .await
            .unwrap()
    }
}

#[test]
fn slice_two_schema_separates_contract_goal_and_runtime_authority() {
    let sql = std::fs::read_to_string("migrations/0013_agent_lifecycle.sql")
        .expect("Slice 2 migration exists");
    for required in [
        "CREATE TABLE goal_states",
        "CREATE TABLE agent_runtime_snapshots",
        "CREATE TABLE agent_transitions",
        "CREATE TABLE agent_commands",
        "CREATE TABLE agent_policy_decisions",
        "CREATE TABLE goal_evidence",
        "CREATE TABLE goal_criterion_assertions",
        "CREATE TABLE goal_blockers",
        "CREATE TABLE agent_task_actions",
        "CREATE TABLE agent_task_events",
        "reject_agent_history_mutation",
    ] {
        assert!(sql.contains(required), "missing schema: {required}");
    }
    assert!(!sql.contains("ALTER TABLE goal_contracts ADD COLUMN phase"));
    assert!(!sql.contains("provider"));
}

#[tokio::test]
async fn migrated_accepted_task_has_independent_initial_axes() {
    let Some(fixture) = AgentStoreFixture::accepted().await else {
        return;
    };
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Accepted);
    assert_eq!(view.operational_status, OperationalStatus::NotStarted);
    assert_eq!(view.task_revision, 0);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(view.goal_revision, 0);
    assert!(view.runtime.is_none());
    assert!(view.blocker.is_none());
}

#[tokio::test]
async fn load_start_seed_returns_the_frozen_agent_definition() {
    let Some(fixture) = AgentStoreFixture::accepted().await else {
        return;
    };
    let seed = fixture
        .store
        .load_agent_start_seed(fixture.session.id.clone(), fixture.task.id.clone())
        .await
        .unwrap();
    assert_eq!(seed.execution.task, fixture.task);
    assert_eq!(seed.definition.dsl_version, 2);
    assert_eq!(
        seed.definition.definition_hash,
        fixture.task.definition.definition_hash
    );
    assert_eq!(seed.definition.tool_bindings.len(), 1);
    assert_eq!(
        seed.definition.tool_bindings[0].qualified_name,
        "docs/search"
    );
}

#[tokio::test]
async fn missing_goal_state_is_rejected() {
    let Some(fixture) = AgentStoreFixture::accepted().await else {
        return;
    };
    sqlx::query("DELETE FROM goal_states WHERE task_id = $1")
        .bind(fixture.task.id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .store
            .get_agent_execution(fixture.session.id, fixture.task.id)
            .await,
        Err(StoreError::InvalidData(message)) if message == "goal state is absent",
    ));
}

#[tokio::test]
async fn stored_runtime_snapshot_integrity_mismatches_are_rejected() {
    for statement in [
        "UPDATE agent_runtime_snapshots SET snapshot_digest = 'sha256:tampered' WHERE task_id = $1",
        "UPDATE agent_runtime_snapshots SET canonical_bytes = canonical_bytes + 1 WHERE task_id = $1",
        "UPDATE agent_runtime_snapshots SET task_revision = task_revision + 1 WHERE task_id = $1",
        "UPDATE agent_runtime_snapshots SET ceiling_bytes = 1 WHERE task_id = $1",
    ] {
        let Some(fixture) = AgentStoreFixture::with_history_rows().await else {
            return;
        };
        sqlx::query(statement)
            .bind(fixture.task.id.as_uuid())
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert!(matches!(
            fixture
                .store
                .get_agent_execution(fixture.session.id, fixture.task.id)
                .await,
            Err(StoreError::RuntimeStateInvalid(message))
                if message == "stored snapshot integrity check failed",
        ));
    }
}

#[tokio::test]
async fn compiled_agent_hash_mismatch_is_rejected() {
    let Some(fixture) = AgentStoreFixture::accepted().await else {
        return;
    };
    sqlx::query("UPDATE workflow_definitions SET definition_hash = $1 WHERE id = $2")
        .bind(format!("sha256:tampered-{}", Uuid::now_v7()))
        .bind(fixture.task.definition.id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .store
            .load_agent_start_seed(fixture.session.id, fixture.task.id)
            .await,
        Err(StoreError::InvalidData(message)) if message == "stored agent definition is malformed",
    ));
}

#[tokio::test]
async fn sql_state_and_runtime_envelope_constraints_are_closed() {
    let Some(fixture) = AgentStoreFixture::with_history_rows().await else {
        return;
    };
    for statement in [
        "UPDATE user_tasks SET state = 'unknown' WHERE id = $1",
        "UPDATE user_tasks SET operational_status = 'unknown' WHERE id = $1",
        "UPDATE goal_states SET phase = 'unknown' WHERE task_id = $1",
        "UPDATE agent_runtime_snapshots SET schema_version = 2 WHERE task_id = $1",
        "UPDATE agent_runtime_snapshots SET runtime_id = 'unknown' WHERE task_id = $1",
        "UPDATE agent_runtime_snapshots SET disposition = 'unknown' WHERE task_id = $1",
        "UPDATE agent_commands SET state = 'unknown' WHERE task_id = $1",
    ] {
        let error = sqlx::query(statement)
            .bind(fixture.task.id.as_uuid())
            .execute(&fixture.pool)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23514"),
            "constraint did not reject {statement}",
        );
    }
    let error = sqlx::query(
        "INSERT INTO agent_task_events \
         (task_id, sequence, kind, metadata, task_revision, goal_revision) \
         VALUES ($1, 2, 'unknown', '{}'::jsonb, 1, 0)",
    )
    .bind(fixture.task.id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
}

#[tokio::test]
async fn immutable_agent_history_rejects_direct_mutation() {
    let Some(fixture) = AgentStoreFixture::with_history_rows().await else {
        return;
    };
    for statement in [
        "UPDATE agent_transitions SET request_digest = 'x' WHERE task_id = $1",
        "DELETE FROM agent_policy_decisions WHERE task_id = $1",
        "DELETE FROM goal_evidence WHERE task_id = $1",
        "DELETE FROM goal_criterion_assertions WHERE task_id = $1",
        "UPDATE goal_blockers SET explanation = 'x' WHERE task_id = $1",
        "DELETE FROM agent_task_actions WHERE task_id = $1",
        "DELETE FROM agent_task_events WHERE task_id = $1",
    ] {
        let error = sqlx::query(statement)
            .bind(fixture.task.id.as_uuid())
            .execute(&fixture.pool)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("55000")
        );
    }
}

#[tokio::test]
async fn agent_task_events_are_typed_and_cursor_ordered() {
    let Some(fixture) = AgentStoreFixture::with_history_rows().await else {
        return;
    };
    let events = fixture
        .store
        .agent_task_events_after(fixture.session.id, fixture.task.id, 0, 16)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sequence, 1);
    assert_eq!(events[0].kind, AgentTaskEventKind::AgentStarted);
    assert_eq!(events[0].task_revision, 1);
    assert_eq!(events[0].goal_revision, 0);
}

#[tokio::test]
async fn session_close_cancels_task_without_completing_goal() {
    let Some(fixture) = AgentStoreFixture::accepted().await else {
        return;
    };
    fixture
        .store
        .close_session_and_cancel_work(fixture.session.id.clone(), &fixture.principal)
        .await
        .unwrap();
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(view.operational_status, OperationalStatus::Finished);
    assert_eq!(view.task_revision, 1);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(view.goal_revision, 0);
}

#[tokio::test]
async fn session_close_advances_snapshot_and_cancels_outstanding_commands() {
    let Some(fixture) = AgentStoreFixture::with_history_rows().await else {
        return;
    };
    fixture
        .store
        .close_session_and_cancel_work(fixture.session.id.clone(), &fixture.principal)
        .await
        .unwrap();
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(view.task_revision, 2);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!(view.goal_revision, 0);
    assert!(view.runtime.is_some());
    let snapshot_revision: i64 =
        sqlx::query_scalar("SELECT task_revision FROM agent_runtime_snapshots WHERE task_id = $1")
            .bind(fixture.task.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(snapshot_revision, 2);
    let command: (String, i64) =
        sqlx::query_as("SELECT state, revision FROM agent_commands WHERE task_id = $1")
            .bind(fixture.task.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(command, ("cancelled".to_owned(), 1));
    let events = fixture
        .store
        .agent_task_events_after(fixture.session.id, fixture.task.id, 0, 16)
        .await
        .unwrap();
    assert_eq!(
        events
            .into_iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec![
            AgentTaskEventKind::AgentStarted,
            AgentTaskEventKind::TaskCancelled,
        ]
    );
}

fn compiled_agent(catalog: &ToolCatalogSnapshot) -> CompiledDefinition {
    let name = format!("babysit_pr_{}", Uuid::now_v7().simple());
    compile(
        &format!(
            r#"agent {name}(repository: string) -> string {{
  entry inspect(repository = repository)
  events ["github.pull_request.changed.v1"]
  limits {{ steps: 20 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task inspect(repository: string) -> string {{
    model reasoning
    prompt """Inspect the pull request."""
    tools [search]
    on finished(summary: string) {{ done summary }}
  }}
}}"#
        ),
        catalog,
        &Limits::default(),
    )
    .unwrap()
}
