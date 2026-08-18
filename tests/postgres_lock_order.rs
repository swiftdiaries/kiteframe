use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    AgentKernel, AgentRuntime, AgentRuntimeError, AgentSelector, CancelUserTask, CatalogTool,
    ClaimAgentCommand, ClosedSchema, CompleteAgentCommand, CreateSession, EffectKind, EvidenceId,
    FrameCommand, FrameId, GoalContract, GoalPhase, IdempotencyKey, KernelCommand,
    KernelCommandPayload, KiteAgentRuntime, NewChild, NewPermissionRequest, OperationalStatus,
    PauseUserTask, PermissionRequestId, PolicyRevision, PolicyUpdate, PostgresStore, PrincipalId,
    RecoverySupport, RequestContext, ResumeUserTask, RunId, RuntimeCeilings, RuntimeCommandKey,
    RuntimeDisposition, RuntimeEvent, RuntimeStart, RuntimeStateSnapshot, RuntimeTransition,
    SessionId, StartAgent, StartRun, StoreError, SubmitUserTask, SuccessCriterion, TaskLifecycle,
    TaskOwnerAuthorizer, TaskPreferences, TaskTarget, ToolCatalogSnapshot, UserTaskId,
};
use serde_json::json;
use sqlx::Row;
use time::{Duration as TimeDuration, OffsetDateTime};
use uuid::Uuid;

struct ChildRun {
    store: Arc<PostgresStore>,
    application_name: String,
    principal: PrincipalId,
    session_id: SessionId,
    run_id: RunId,
    parent_id: FrameId,
    child_id: FrameId,
    sibling_id: FrameId,
    descriptor_digest: String,
}

#[test]
fn frozen_runtime_reference_pins_global_postgres_lock_order() {
    let reference = include_str!("../docs/reference/agent-runtime-and-goal-lifecycle.md");
    let normalized = reference.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(
        normalized.contains(
            "session -> user task -> goal state -> frozen definition -> runtime snapshot -> all command rows"
        ),
        "runtime reference must preserve the global agent lock order",
    );
    assert!(
        normalized.contains("`AgentCommandId` UUID (`id`) order"),
        "runtime reference must order command-row locks by their stable UUID ids",
    );
    assert!(
        !normalized.contains("Command rows are acquired or inserted in `RuntimeCommandKey` order"),
        "runtime command keys are not the PostgreSQL row-lock order",
    );
}

async fn child_run(application_name: &str) -> ChildRun {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL is required for PostgreSQL lock-order tests");
    let separator = if database_url.contains('?') { '&' } else { '?' };
    let database_url = format!("{database_url}{separator}application_name={application_name}");
    let store = Arc::new(
        PostgresStore::connect(&database_url)
            .await
            .expect("connect tagged store"),
    );
    let principal =
        PrincipalId::new(format!("lock-test-{}", Uuid::now_v7())).expect("valid principal");
    let session = store
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/tmp/kiteframe-lock-test".to_owned(),
            retention_until: OffsetDateTime::now_utc() + TimeDuration::days(1),
        })
        .await
        .expect("create session");
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
        .expect("test tool")],
    )
    .expect("tool catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("append catalog");
    let workflow_name = format!("lock{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {workflow_name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok(value: string) {{ done value }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("compile workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish workflow");
    let run = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name,
            version: Some(definition.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new(format!("start-{}", Uuid::now_v7()))
                .expect("valid idempotency key"),
        })
        .await
        .expect("start run");
    store
        .apply_frame_command(
            run.root_frame_id.clone(),
            0,
            FrameCommand::StartParallel(vec![
                NewChild {
                    target: "t".to_owned(),
                    binding: "child_result".to_owned(),
                    arguments: Default::default(),
                    branch_index: 0,
                },
                NewChild {
                    target: "t".to_owned(),
                    binding: "sibling_result".to_owned(),
                    arguments: Default::default(),
                    branch_index: 1,
                },
            ]),
        )
        .await
        .expect("start child");
    let child_id: Uuid =
        sqlx::query_scalar("SELECT id FROM frames WHERE parent_frame_id = $1 AND branch_index = 0")
            .bind(*run.root_frame_id.as_uuid())
            .fetch_one(store.pool())
            .await
            .expect("find child");
    let child_id = FrameId::new(child_id);
    let sibling_id: Uuid =
        sqlx::query_scalar("SELECT id FROM frames WHERE parent_frame_id = $1 AND branch_index = 1")
            .bind(*run.root_frame_id.as_uuid())
            .fetch_one(store.pool())
            .await
            .expect("find sibling");
    assert!(
        run.root_frame_id.as_uuid() < child_id.as_uuid(),
        "UUID ordering must put the parent before its child"
    );

    ChildRun {
        store,
        application_name: application_name.to_owned(),
        principal,
        session_id: session.id,
        run_id: run.id,
        parent_id: run.root_frame_id,
        child_id,
        sibling_id: FrameId::new(sibling_id),
        descriptor_digest: catalog.tools()["docs/search"]
            .descriptor_digest()
            .to_owned(),
    }
}

async fn accepted_agent_task(child: &ChildRun) -> UserTaskId {
    let name = format!("lock_agent_{}", Uuid::now_v7().simple());
    let source = format!(
        r#"agent {name}(repository: string) -> string {{
  entry inspect(repository = repository)
  events ["github.pull_request.changed.v1"]
  limits {{ steps: 20 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task inspect(repository: string) -> string {{
    model reasoning
    prompt """Inspect the pull request."""
    tools []
    on finished(summary: string) {{ done summary }}
  }}
}}"#
    );
    let catalog = ToolCatalogSnapshot::new(
        child.session_id.clone(),
        Uuid::now_v7(),
        std::iter::empty::<CatalogTool>(),
    )
    .expect("empty agent catalog");
    let agent = compile(&source, &catalog, &Limits::default()).expect("compile lock agent");
    let definition = child
        .store
        .publish(child.session_id.clone(), &agent, &source)
        .await
        .expect("publish lock agent");
    child
        .store
        .submit_user_task(
            &child.principal,
            SubmitUserTask {
                session_id: child.session_id.clone(),
                agent: AgentSelector {
                    name: definition.name,
                    version: Some(definition.version),
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
                    constraints: vec![],
                    blocked_when: vec![],
                },
                idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                    .expect("task idempotency key"),
            },
        )
        .await
        .expect("submit lock agent task")
        .id
}

async fn accepted_fork_agent_task(child: &ChildRun) -> UserTaskId {
    let name = format!("lock_fork_agent_{}", Uuid::now_v7().simple());
    let source = format!(
        r#"agent {name}(repository: string, pull_request: integer) -> string {{
  entry root(repository = repository, pull_request = pull_request)
  limits {{ steps: 100 model_turns_per_task: 10 tool_calls_per_response: 4 parallel_branches: 4 response_item_bytes: 16384 active_execution_seconds: 60 elapsed_seconds: 600 }}
  task root(repository: string, pull_request: integer) -> string {{
    model reasoning prompt """Split.""" tools []
    on split(left: string, right: string) {{
      fork {{
        first = call branch(value = left) context isolated
        second = call branch(value = right) context kept
      }} join
      done first
    }}
  }}
  task branch(value: string) -> string {{
    model reasoning prompt """Return.""" tools []
    on finished(summary: string) {{ done summary }}
  }}
}}"#
    );
    let catalog = ToolCatalogSnapshot::new(
        child.session_id.clone(),
        Uuid::now_v7(),
        std::iter::empty::<CatalogTool>(),
    )
    .expect("empty fork-agent catalog");
    let agent = compile(&source, &catalog, &Limits::default()).expect("compile fork lock agent");
    let definition = child
        .store
        .publish(child.session_id.clone(), &agent, &source)
        .await
        .expect("publish fork lock agent");
    child
        .store
        .submit_user_task(
            &child.principal,
            SubmitUserTask {
                session_id: child.session_id.clone(),
                agent: AgentSelector {
                    name: definition.name,
                    version: Some(definition.version),
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
                    constraints: vec![],
                    blocked_when: vec![],
                },
                idempotency_key: IdempotencyKey::new(format!("submit-fork-{}", Uuid::now_v7()))
                    .expect("fork task idempotency key"),
            },
        )
        .await
        .expect("submit fork lock agent task")
        .id
}

struct LockStartRuntime;

impl AgentRuntime for LockStartRuntime {
    fn validate_agent(
        &self,
        _definition: &kiteframe::dsl::CompiledDefinition,
    ) -> Result<(), AgentRuntimeError> {
        Ok(())
    }

    fn start(&self, input: RuntimeStart<'_>) -> Result<RuntimeTransition, AgentRuntimeError> {
        Ok(RuntimeTransition {
            idempotency_key: "lock-runtime-start".to_owned(),
            expected_task_revision: input.execution.task_revision,
            expected_goal_revision: input.execution.goal_revision,
            snapshot: RuntimeStateSnapshot::new(
                RuntimeDisposition::Running,
                json!({"lockTest": true}),
                &RuntimeCeilings::default(),
            )?,
            commands: vec![KernelCommand {
                schema_version: 1,
                key: RuntimeCommandKey::new("context-1")?,
                payload: KernelCommandPayload::RequestContext(RequestContext {
                    evidence_type: "github.review_thread".to_owned(),
                    subject: json!({}),
                }),
            }],
        })
    }

    fn handle(
        &self,
        _input: kiteframe::RuntimeHandle<'_>,
    ) -> Result<RuntimeTransition, AgentRuntimeError> {
        unreachable!("lock-order start tests do not handle runtime events")
    }
}

fn agent_kernel(child: &ChildRun) -> Arc<AgentKernel> {
    Arc::new(AgentKernel::new(
        Arc::clone(&child.store),
        Arc::new(LockStartRuntime),
        Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&child.store))),
    ))
}

fn kite_agent_kernel(child: &ChildRun) -> Arc<AgentKernel> {
    Arc::new(AgentKernel::new(
        Arc::clone(&child.store),
        Arc::new(KiteAgentRuntime::new(RuntimeCeilings::default())),
        Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&child.store))),
    ))
}

fn start_agent_request(child: &ChildRun, task_id: UserTaskId, key: &str) -> StartAgent {
    StartAgent {
        session_id: child.session_id.clone(),
        task_id,
        expected_task_revision: 0,
        expected_goal_revision: 0,
        idempotency_key: IdempotencyKey::new(key).expect("valid start key"),
    }
}

async fn wait_for_blocked_connections(store: &PostgresStore, application_name: &str, minimum: i64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked = blocked_connection_count(store, application_name).await;
            if blocked >= minimum {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("expected at least {minimum} blocked {application_name} connections")
    });
}

async fn blocked_connection_count(store: &PostgresStore, application_name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_stat_activity WHERE datname = current_database() AND application_name = $1 AND wait_event_type = 'Lock' AND cardinality(pg_blocking_pids(pid)) > 0",
    )
    .bind(application_name)
    .fetch_one(store.pool())
    .await
    .expect("inspect lock waiters")
}

async fn wait_for_blocked_or_finished(
    store: &PostgresStore,
    application_name: &str,
    worker: &tokio::task::JoinHandle<Result<u64, StoreError>>,
) -> bool {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if worker.is_finished() {
                return false;
            }
            if blocked_connection_count(store, application_name).await >= 1 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recovery worker must either finish or wait on the gate")
}

async fn assert_cancelled(child: &ChildRun, session_state: &str) {
    let actual_session: String = sqlx::query_scalar("SELECT state FROM sessions WHERE id = $1")
        .bind(*child.session_id.as_uuid())
        .fetch_one(child.store.pool())
        .await
        .expect("session state");
    assert_eq!(actual_session, session_state);
    let run_state: String = sqlx::query_scalar("SELECT state FROM runs WHERE id = $1")
        .bind(*child.run_id.as_uuid())
        .fetch_one(child.store.pool())
        .await
        .expect("run state");
    assert_eq!(run_state, "cancelled");
    let frame_states = sqlx::query("SELECT state FROM frames WHERE run_id = $1 ORDER BY id")
        .bind(*child.run_id.as_uuid())
        .fetch_all(child.store.pool())
        .await
        .expect("frame states")
        .into_iter()
        .map(|row| row.get::<String, _>("state"))
        .collect::<Vec<_>>();
    assert_eq!(frame_states.len(), 3);
    assert!(frame_states.iter().all(|state| state == "cancelled"));
    let cancellation_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE run_id = $1 AND kind = 'run.cancelled'",
    )
    .bind(*child.run_id.as_uuid())
    .fetch_one(child.store.pool())
    .await
    .expect("cancellation event count");
    assert_eq!(cancellation_events, 1);
}

async fn assert_transition_locks_run_before_frame<F, Fut>(child: &ChildRun, transition: F)
where
    F: FnOnce(Arc<PostgresStore>, FrameId) -> Fut,
    Fut: Future<Output = Result<(), StoreError>> + Send + 'static,
{
    let mut run_gate = child.store.pool().begin().await.expect("run gate");
    sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE")
        .bind(*child.run_id.as_uuid())
        .execute(&mut *run_gate)
        .await
        .expect("lock run");

    let mut transition = tokio::spawn(transition(Arc::clone(&child.store), child.child_id.clone()));
    wait_for_blocked_connections(&child.store, &child.application_name, 1).await;

    let mut frame_probe = child.store.pool().begin().await.expect("frame probe");
    let frame_result = sqlx::query("SELECT id FROM frames WHERE id = $1 FOR UPDATE NOWAIT")
        .bind(*child.child_id.as_uuid())
        .execute(&mut *frame_probe)
        .await;
    assert!(
        frame_result.is_ok(),
        "transition locked its frame before the run: {frame_result:?}"
    );
    frame_probe.rollback().await.expect("release frame probe");
    run_gate.commit().await.expect("release run gate");

    let transition_result = tokio::time::timeout(Duration::from_secs(5), &mut transition)
        .await
        .expect("transition must terminate")
        .expect("transition task");
    assert!(
        transition_result.is_ok(),
        "transition failed: {transition_result:?}"
    );
    child
        .store
        .cancel_run(
            child.run_id.clone(),
            0,
            Some("lock-order test cleanup".to_owned()),
            format!("cleanup-{}", Uuid::now_v7()),
        )
        .await
        .expect("cancel transition fixture");
}

#[tokio::test]
async fn child_completion_and_session_close_use_the_same_frame_lock_order() {
    let application_name = format!("kf-close-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let mut parent_gate = child.store.pool().begin().await.expect("parent gate");
    sqlx::query("SELECT id FROM frames WHERE id = $1 FOR UPDATE")
        .bind(*child.parent_id.as_uuid())
        .execute(&mut *parent_gate)
        .await
        .expect("lock parent");

    let close_store = Arc::clone(&child.store);
    let close_session = child.session_id.clone();
    let close_principal = child.principal.clone();
    let mut close = tokio::spawn(async move {
        close_store
            .close_session_and_cancel_work(close_session, &close_principal)
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let completion_store = Arc::clone(&child.store);
    let completion_child = child.child_id.clone();
    let mut completion = tokio::spawn(async move {
        completion_store
            .apply_frame_command(
                completion_child,
                0,
                FrameCommand::Complete(json!("child done")),
            )
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 2).await;
    parent_gate.commit().await.expect("release parent gate");

    let (close_result, completion_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(&mut close, &mut completion)
    })
    .await
    .expect("close and completion must terminate");
    let close_result = close_result.expect("close task");
    assert!(close_result.is_ok(), "close failed: {close_result:?}");
    let completion_result = completion_result.expect("completion task");
    assert!(
        matches!(completion_result, Err(StoreError::RevisionConflict)),
        "completion failed: {completion_result:?}"
    );
    assert_cancelled(&child, "closed").await;
}

#[tokio::test]
async fn session_close_locks_session_before_ordered_task_and_goal_rows() {
    let application_name = format!("kf-agent-close-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let task_id = accepted_agent_task(&child).await;
    let mut task_goal_gate = child.store.pool().begin().await.expect("task/goal gate");
    sqlx::query("SELECT id FROM user_tasks WHERE id = $1 FOR UPDATE")
        .bind(task_id.as_uuid())
        .execute(&mut *task_goal_gate)
        .await
        .expect("lock task");
    sqlx::query("SELECT task_id FROM goal_states WHERE task_id = $1 FOR UPDATE")
        .bind(task_id.as_uuid())
        .execute(&mut *task_goal_gate)
        .await
        .expect("lock goal");

    let close_store = Arc::clone(&child.store);
    let close_session = child.session_id.clone();
    let close_principal = child.principal.clone();
    let mut close = tokio::spawn(async move {
        close_store
            .close_session_and_cancel_work(close_session, &close_principal)
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let mut session_probe = child.store.pool().begin().await.expect("session probe");
    let session_lock = sqlx::query("SELECT id FROM sessions WHERE id = $1 FOR UPDATE NOWAIT")
        .bind(child.session_id.as_uuid())
        .execute(&mut *session_probe)
        .await
        .expect_err("close must hold the session prefix before waiting on the task");
    assert_eq!(
        session_lock.as_database_error().unwrap().code().as_deref(),
        Some("55P03"),
    );
    session_probe
        .rollback()
        .await
        .expect("release session probe");
    task_goal_gate
        .commit()
        .await
        .expect("release task/goal gate");

    tokio::time::timeout(Duration::from_secs(5), &mut close)
        .await
        .expect("session close must terminate")
        .expect("session close task")
        .expect("session close result");
    assert_eq!(
        child
            .store
            .get_agent_execution(child.session_id, task_id)
            .await
            .expect("cancelled task")
            .lifecycle,
        TaskLifecycle::Cancelled,
    );
}

#[tokio::test]
async fn session_close_and_principal_actions_share_the_session_task_goal_prefix() {
    for action in ["pause", "resume", "cancel"] {
        let application_name = format!("kf-{action}-close-lock-{}", Uuid::now_v7());
        let child = child_run(&application_name).await;
        let task_id = accepted_agent_task(&child).await;
        let kernel = agent_kernel(&child);
        let mut current = kernel
            .start_agent(
                &child.principal,
                start_agent_request(&child, task_id.clone(), &format!("start-{action}-close")),
            )
            .await
            .expect("start agent task");
        if action == "resume" {
            current = kernel
                .pause_task(
                    &child.principal,
                    PauseUserTask {
                        session_id: child.session_id.clone(),
                        task_id: task_id.clone(),
                        expected_task_revision: current.task_revision,
                        expected_goal_revision: current.goal_revision,
                        reason: "Pause before the close race.".to_owned(),
                        idempotency_key: IdempotencyKey::new("pause-before-close-race").unwrap(),
                    },
                )
                .await
                .expect("pause before resume race");
        }

        let mut task_goal_gate = child.store.pool().begin().await.expect("task/goal gate");
        sqlx::query("SELECT id FROM user_tasks WHERE id = $1 FOR UPDATE")
            .bind(task_id.as_uuid())
            .execute(&mut *task_goal_gate)
            .await
            .expect("lock task");
        sqlx::query("SELECT task_id FROM goal_states WHERE task_id = $1 FOR UPDATE")
            .bind(task_id.as_uuid())
            .execute(&mut *task_goal_gate)
            .await
            .expect("lock goal");

        let close_store = Arc::clone(&child.store);
        let close_session = child.session_id.clone();
        let close_principal = child.principal.clone();
        let mut close = tokio::spawn(async move {
            close_store
                .close_session_and_cancel_work(close_session, &close_principal)
                .await
        });
        wait_for_blocked_connections(&child.store, &application_name, 1).await;

        let action_kernel = Arc::clone(&kernel);
        let action_principal = child.principal.clone();
        let action_session = child.session_id.clone();
        let action_task_id = task_id.clone();
        let mut principal_action = tokio::spawn(async move {
            match action {
                "pause" => {
                    action_kernel
                        .pause_task(
                            &action_principal,
                            PauseUserTask {
                                session_id: action_session,
                                task_id: action_task_id,
                                expected_task_revision: current.task_revision,
                                expected_goal_revision: current.goal_revision,
                                reason: "Pause during close.".to_owned(),
                                idempotency_key: IdempotencyKey::new("pause-during-close").unwrap(),
                            },
                        )
                        .await
                }
                "resume" => {
                    action_kernel
                        .resume_task(
                            &action_principal,
                            ResumeUserTask {
                                session_id: action_session,
                                task_id: action_task_id,
                                expected_task_revision: current.task_revision,
                                expected_goal_revision: current.goal_revision,
                                idempotency_key: IdempotencyKey::new("resume-during-close")
                                    .unwrap(),
                            },
                        )
                        .await
                }
                "cancel" => {
                    action_kernel
                        .cancel_task(
                            &action_principal,
                            CancelUserTask {
                                session_id: action_session,
                                task_id: action_task_id,
                                expected_task_revision: current.task_revision,
                                expected_goal_revision: current.goal_revision,
                                reason: Some("Cancel during close.".to_owned()),
                                idempotency_key: IdempotencyKey::new("cancel-during-close")
                                    .unwrap(),
                            },
                        )
                        .await
                }
                _ => unreachable!(),
            }
        });
        wait_for_blocked_connections(&child.store, &application_name, 2).await;
        task_goal_gate
            .commit()
            .await
            .expect("release task/goal gate");

        let (close_result, action_result) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(&mut close, &mut principal_action)
        })
        .await
        .expect("session close and principal action must not deadlock");
        close_result
            .expect("session close task")
            .expect("session close result");
        assert!(
            matches!(
                action_result.expect("principal action task"),
                Err(StoreError::SessionClosed)
            ),
            "{action} must lose to the session close",
        );
        assert_eq!(
            child
                .store
                .get_agent_execution(child.session_id.clone(), task_id)
                .await
                .expect("cancelled agent task")
                .lifecycle,
            TaskLifecycle::Cancelled,
        );
        assert_cancelled(&child, "closed").await;
    }
}

#[tokio::test]
async fn agent_start_and_session_close_share_the_session_task_goal_prefix() {
    let application_name = format!("kf-start-close-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let task_id = accepted_agent_task(&child).await;
    let kernel = agent_kernel(&child);
    let mut goal_gate = child.store.pool().begin().await.expect("goal gate");
    sqlx::query("SELECT task_id FROM goal_states WHERE task_id = $1 FOR UPDATE")
        .bind(task_id.as_uuid())
        .execute(&mut *goal_gate)
        .await
        .expect("lock goal");

    let start_principal = child.principal.clone();
    let start_request = start_agent_request(&child, task_id.clone(), "start-close-lock");
    let mut start =
        tokio::spawn(async move { kernel.start_agent(&start_principal, start_request).await });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let close_store = Arc::clone(&child.store);
    let close_session = child.session_id.clone();
    let close_principal = child.principal.clone();
    let mut close = tokio::spawn(async move {
        close_store
            .close_session_and_cancel_work(close_session, &close_principal)
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 2).await;

    let mut session_probe = child.store.pool().begin().await.expect("session probe");
    let session_lock = sqlx::query("SELECT id FROM sessions WHERE id = $1 FOR UPDATE NOWAIT")
        .bind(child.session_id.as_uuid())
        .execute(&mut *session_probe)
        .await
        .expect_err("start must retain the session prefix while waiting on the goal");
    assert_eq!(
        session_lock.as_database_error().unwrap().code().as_deref(),
        Some("55P03"),
    );
    session_probe
        .rollback()
        .await
        .expect("release session probe");
    goal_gate.commit().await.expect("release goal gate");

    let (start_result, close_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(&mut start, &mut close)
    })
    .await
    .expect("start and close must terminate without deadlock");
    start_result
        .expect("start task")
        .expect("start transaction");
    close_result
        .expect("close task")
        .expect("close transaction");
    assert_cancelled(&child, "closed").await;
}

#[tokio::test]
async fn agent_start_does_not_lock_the_definition_before_the_goal() {
    let application_name = format!("kf-start-definition-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let task_id = accepted_agent_task(&child).await;
    let definition_id: Uuid =
        sqlx::query_scalar("SELECT definition_id FROM user_tasks WHERE id = $1")
            .bind(task_id.as_uuid())
            .fetch_one(child.store.pool())
            .await
            .expect("find frozen definition");
    let kernel = agent_kernel(&child);
    let mut goal_gate = child.store.pool().begin().await.expect("goal gate");
    sqlx::query("SELECT task_id FROM goal_states WHERE task_id = $1 FOR UPDATE")
        .bind(task_id.as_uuid())
        .execute(&mut *goal_gate)
        .await
        .expect("lock goal");

    let start_principal = child.principal.clone();
    let start_request = start_agent_request(&child, task_id, "definition-after-goal");
    let mut start =
        tokio::spawn(async move { kernel.start_agent(&start_principal, start_request).await });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let mut definition_probe = child.store.pool().begin().await.expect("definition probe");
    let definition_lock =
        sqlx::query("SELECT id FROM workflow_definitions WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(definition_id)
            .execute(&mut *definition_probe)
            .await;
    assert!(
        definition_lock.is_ok(),
        "start locked the definition before acquiring the goal: {definition_lock:?}"
    );
    definition_probe
        .rollback()
        .await
        .expect("release definition probe");
    goal_gate.commit().await.expect("release goal gate");

    tokio::time::timeout(Duration::from_secs(5), &mut start)
        .await
        .expect("start must terminate")
        .expect("start task")
        .expect("start result");
}

#[tokio::test]
async fn concurrent_agent_starts_do_not_invert_the_session_task_goal_prefix() {
    let application_name = format!("kf-start-prefix-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let task_id = accepted_agent_task(&child).await;
    let kernel = agent_kernel(&child);
    let mut goal_gate = child.store.pool().begin().await.expect("goal gate");
    sqlx::query("SELECT task_id FROM goal_states WHERE task_id = $1 FOR UPDATE")
        .bind(task_id.as_uuid())
        .execute(&mut *goal_gate)
        .await
        .expect("lock goal");

    let first_kernel = kernel.clone();
    let first_principal = child.principal.clone();
    let request = start_agent_request(&child, task_id, "same-prefix-start");
    let first_request = request.clone();
    let mut first = tokio::spawn(async move {
        first_kernel
            .start_agent(&first_principal, first_request)
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let second_principal = child.principal.clone();
    let mut second =
        tokio::spawn(async move { kernel.start_agent(&second_principal, request).await });
    wait_for_blocked_connections(&child.store, &application_name, 2).await;
    goal_gate.commit().await.expect("release goal gate");

    let (first_result, second_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(&mut first, &mut second)
    })
    .await
    .expect("concurrent starts must terminate without deadlock");
    let first_result = first_result.expect("first task").expect("first start");
    let second_result = second_result.expect("second task").expect("second start");
    assert_eq!(first_result.task.id, second_result.task.id);
}

#[tokio::test]
async fn pause_winning_the_aggregate_lock_prevents_a_stale_pending_claim() {
    let application_name = format!("kf-pause-claim-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let task_id = accepted_fork_agent_task(&child).await;
    let kernel = kite_agent_kernel(&child);
    let started = kernel
        .start_agent(
            &child.principal,
            start_agent_request(&child, task_id.clone(), "start-pause-claim"),
        )
        .await
        .expect("start agent");
    let pending = child
        .store
        .dispatchable_agent_commands(256)
        .await
        .expect("list pending command")
        .into_iter()
        .find(|command| command.task_id == task_id)
        .expect("pending command");

    let mut snapshot_gate = child.store.pool().begin().await.expect("snapshot gate");
    sqlx::query("SELECT task_id FROM agent_runtime_snapshots WHERE task_id = $1 FOR UPDATE")
        .bind(task_id.as_uuid())
        .execute(&mut *snapshot_gate)
        .await
        .expect("lock runtime snapshot");

    let pause_kernel = Arc::clone(&kernel);
    let pause_principal = child.principal.clone();
    let pause_session = child.session_id.clone();
    let pause_task = task_id.clone();
    let mut pause = tokio::spawn(async move {
        pause_kernel
            .pause_task(
                &pause_principal,
                PauseUserTask {
                    session_id: pause_session,
                    task_id: pause_task,
                    expected_task_revision: started.task_revision,
                    expected_goal_revision: started.goal_revision,
                    reason: "Pause before dispatch.".to_owned(),
                    idempotency_key: IdempotencyKey::new("pause-before-claim").expect("pause key"),
                },
            )
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let claim_store = Arc::clone(&child.store);
    let claim_session = child.session_id.clone();
    let claim_task = task_id.clone();
    let mut claim = tokio::spawn(async move {
        claim_store
            .claim_agent_command(ClaimAgentCommand {
                session_id: claim_session,
                task_id: claim_task,
                command_id: pending.id,
                expected_command_revision: pending.revision,
                claimant: "builtin-direct-v1".to_owned(),
                idempotency_key: IdempotencyKey::new("claim-after-pause-list").expect("claim key"),
            })
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 2).await;
    snapshot_gate.commit().await.expect("release snapshot gate");

    let (pause_result, claim_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(&mut pause, &mut claim)
    })
    .await
    .expect("pause and claim must terminate without deadlock");
    let pause_result = pause_result.expect("pause task");
    let claim_result = claim_result.expect("claim task");
    assert_eq!(
        usize::from(pause_result.is_ok()) + usize::from(claim_result.is_ok()),
        1,
        "exactly one aggregate mutation may commit",
    );
    let paused = pause_result.expect("pause wins the gated aggregate lock");
    assert!(matches!(claim_result, Err(StoreError::RevisionConflict)));
    assert_eq!(paused.goal_phase, GoalPhase::Paused);
    assert_eq!(paused.operational_status, OperationalStatus::Paused);
    let (state, revision): (String, i64) =
        sqlx::query_as("SELECT state, revision FROM agent_commands WHERE task_id = $1")
            .bind(task_id.as_uuid())
            .fetch_one(child.store.pool())
            .await
            .expect("command state");
    assert_eq!(state, "pending");
    assert_eq!(revision, 0);
    assert!(
        child
            .store
            .dispatchable_agent_commands(256)
            .await
            .expect("dispatch after pause")
            .into_iter()
            .all(|command| command.task_id != task_id),
    );
}

#[tokio::test]
async fn session_close_locks_multiple_agent_commands_in_id_order() {
    let application_name = format!("kf-agent-command-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let task_id = accepted_agent_task(&child).await;
    let transition_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agent_transitions \
         (id, task_id, idempotency_key, request_digest, expected_task_revision, \
          expected_goal_revision, committed_task_revision, committed_goal_revision, \
          snapshot_digest, command_digests) \
         VALUES ($1, $2, $3, 'sha256:request', 0, 0, 1, 0, \
                 'sha256:snapshot', '[]'::jsonb)",
    )
    .bind(transition_id)
    .bind(task_id.as_uuid())
    .bind(format!("transition-{}", Uuid::now_v7()))
    .execute(child.store.pool())
    .await
    .expect("insert transition");

    let low = Uuid::now_v7();
    let middle = Uuid::now_v7();
    let high = Uuid::now_v7();
    assert!(
        low < middle && middle < high,
        "UUIDs must define lock order"
    );
    for (ordinal, id, key, state) in [
        (0_i32, high, "command-high", "pending"),
        (1_i32, middle, "command-middle", "claimed"),
        (2_i32, low, "command-low", "pending"),
    ] {
        sqlx::query(
            "INSERT INTO agent_commands \
             (id, task_id, transition_id, ordinal, schema_version, runtime_key, \
              command_kind, payload, payload_digest, state) \
             VALUES ($1, $2, $3, $4, 1, $5, 'request_context', '{}'::jsonb, $6, $7)",
        )
        .bind(id)
        .bind(task_id.as_uuid())
        .bind(transition_id)
        .bind(ordinal)
        .bind(key)
        .bind(format!("sha256:{key}"))
        .bind(state)
        .execute(child.store.pool())
        .await
        .expect("insert command");
    }

    let mut low_gate = child.store.pool().begin().await.expect("low command gate");
    sqlx::query("SELECT id FROM agent_commands WHERE id = $1 FOR UPDATE")
        .bind(low)
        .execute(&mut *low_gate)
        .await
        .expect("lock lowest command");

    let close_store = Arc::clone(&child.store);
    let close_session = child.session_id.clone();
    let close_principal = child.principal.clone();
    let mut close = tokio::spawn(async move {
        close_store
            .close_session_and_cancel_work(close_session, &close_principal)
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let mut later_probe = child
        .store
        .pool()
        .begin()
        .await
        .expect("later command probe");
    let later_result = sqlx::query(
        "SELECT id FROM agent_commands WHERE id = ANY($1) ORDER BY id FOR UPDATE NOWAIT",
    )
    .bind(vec![middle, high])
    .fetch_all(&mut *later_probe)
    .await;
    assert!(
        later_result.is_ok(),
        "session close locked a later command before the lowest command: {later_result:?}",
    );
    later_probe
        .rollback()
        .await
        .expect("release later command probe");
    low_gate.commit().await.expect("release low command gate");

    tokio::time::timeout(Duration::from_secs(5), &mut close)
        .await
        .expect("session close must terminate")
        .expect("session close task")
        .expect("session close result");
    let command_states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM agent_commands WHERE task_id = $1 ORDER BY id")
            .bind(task_id.as_uuid())
            .fetch_all(child.store.pool())
            .await
            .expect("command states");
    assert_eq!(
        command_states,
        vec![
            "cancelled".to_owned(),
            "cancelled".to_owned(),
            "cancelled".to_owned(),
        ],
    );
}

#[tokio::test]
async fn command_completion_and_session_close_share_ordered_command_locks() {
    let application_name = format!("kf-complete-close-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let task_id = accepted_fork_agent_task(&child).await;
    let kernel = kite_agent_kernel(&child);
    kernel
        .start_agent(
            &child.principal,
            start_agent_request(&child, task_id.clone(), "start-completion-lock"),
        )
        .await
        .expect("start fork agent");
    let pending_root = child
        .store
        .dispatchable_agent_commands(256)
        .await
        .expect("dispatch root")
        .into_iter()
        .find(|command| command.task_id == task_id)
        .expect("root command");
    let root = child
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: child.session_id.clone(),
            task_id: task_id.clone(),
            command_id: pending_root.id,
            expected_command_revision: pending_root.revision,
            claimant: "builtin-direct-v1".to_owned(),
            idempotency_key: IdempotencyKey::new("claim-lock-root").expect("root claim key"),
        })
        .await
        .expect("claim root command");
    kernel
        .complete_command(
            &child.principal,
            CompleteAgentCommand {
                session_id: child.session_id.clone(),
                task_id: task_id.clone(),
                command_id: root.id,
                expected_command_revision: root.revision,
                idempotency_key: IdempotencyKey::new("complete-lock-root")
                    .expect("root completion key"),
                event: RuntimeEvent::HarnessResult {
                    event_id: "evt-lock-root".to_owned(),
                    command_key: root.key,
                    outcome: "split".to_owned(),
                    fields: json!({"left":"one", "right":"two"}),
                    evidence_id: EvidenceId::new(Uuid::now_v7()),
                    history: vec![],
                },
            },
        )
        .await
        .expect("split root");
    let pending_source = child
        .store
        .dispatchable_agent_commands(256)
        .await
        .expect("dispatch children")
        .into_iter()
        .filter(|command| command.task_id == task_id)
        .max_by_key(|command| *command.id.as_uuid())
        .expect("child command");
    let source = child
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: child.session_id.clone(),
            task_id: task_id.clone(),
            command_id: pending_source.id,
            expected_command_revision: pending_source.revision,
            claimant: "builtin-direct-v1".to_owned(),
            idempotency_key: IdempotencyKey::new("claim-lock-child").expect("child claim key"),
        })
        .await
        .expect("claim child command");
    let command_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM agent_commands WHERE task_id = $1 ORDER BY id")
            .bind(task_id.as_uuid())
            .fetch_all(child.store.pool())
            .await
            .expect("ordered command ids");
    assert!(command_ids.len() >= 3);
    let lowest = command_ids[0];
    let later = command_ids[1..].to_vec();

    let mut command_gate = child.store.pool().begin().await.expect("command gate");
    sqlx::query("SELECT id FROM agent_commands WHERE id = $1 FOR UPDATE")
        .bind(lowest)
        .execute(&mut *command_gate)
        .await
        .expect("lock lowest command");

    let completion_kernel = Arc::clone(&kernel);
    let completion_principal = child.principal.clone();
    let completion_session = child.session_id.clone();
    let completion_task = task_id.clone();
    let mut completion = tokio::spawn(async move {
        completion_kernel
            .complete_command(
                &completion_principal,
                CompleteAgentCommand {
                    session_id: completion_session,
                    task_id: completion_task,
                    command_id: source.id,
                    expected_command_revision: source.revision,
                    idempotency_key: IdempotencyKey::new("complete-lock-child")
                        .expect("child completion key"),
                    event: RuntimeEvent::HarnessResult {
                        event_id: "evt-lock-child".to_owned(),
                        command_key: source.key,
                        outcome: "finished".to_owned(),
                        fields: json!({"summary":"one"}),
                        evidence_id: EvidenceId::new(Uuid::now_v7()),
                        history: vec![],
                    },
                },
            )
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let mut later_probe = child.store.pool().begin().await.expect("later probe");
    let later_result = sqlx::query(
        "SELECT id FROM agent_commands WHERE id = ANY($1) ORDER BY id FOR UPDATE NOWAIT",
    )
    .bind(&later)
    .fetch_all(&mut *later_probe)
    .await;
    assert!(
        later_result.is_ok(),
        "completion locked a later command before the lowest command: {later_result:?}",
    );
    later_probe.rollback().await.expect("release later probe");

    let close_store = Arc::clone(&child.store);
    let close_session = child.session_id.clone();
    let close_principal = child.principal.clone();
    let mut close = tokio::spawn(async move {
        close_store
            .close_session_and_cancel_work(close_session, &close_principal)
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 2).await;
    command_gate.commit().await.expect("release command gate");

    let (completion_result, close_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(&mut completion, &mut close)
    })
    .await
    .expect("completion and close must terminate without deadlock");
    completion_result
        .expect("completion task")
        .expect("completion result");
    close_result.expect("close task").expect("close result");
    assert_eq!(
        child
            .store
            .get_agent_execution(child.session_id.clone(), task_id)
            .await
            .expect("cancelled agent task")
            .lifecycle,
        TaskLifecycle::Cancelled,
    );
    assert_cancelled(&child, "closed").await;
}

#[tokio::test]
async fn child_completion_and_run_cancel_use_the_same_frame_lock_order() {
    let application_name = format!("kf-cancel-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let mut parent_gate = child.store.pool().begin().await.expect("parent gate");
    sqlx::query("SELECT id FROM frames WHERE id = $1 FOR UPDATE")
        .bind(*child.parent_id.as_uuid())
        .execute(&mut *parent_gate)
        .await
        .expect("lock parent");

    let cancel_store = Arc::clone(&child.store);
    let cancel_run = child.run_id.clone();
    let mut cancel = tokio::spawn(async move {
        cancel_store
            .cancel_run(
                cancel_run,
                0,
                Some("operator request".to_owned()),
                format!("cancel-{}", Uuid::now_v7()),
            )
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let completion_store = Arc::clone(&child.store);
    let completion_child = child.child_id.clone();
    let mut completion = tokio::spawn(async move {
        completion_store
            .apply_frame_command(
                completion_child,
                0,
                FrameCommand::Complete(json!("child done")),
            )
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 2).await;
    parent_gate.commit().await.expect("release parent gate");

    let (cancel_result, completion_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(&mut cancel, &mut completion)
    })
    .await
    .expect("cancel and completion must terminate");
    let cancel_result = cancel_result.expect("cancel task");
    assert!(cancel_result.is_ok(), "cancel failed: {cancel_result:?}");
    let completion_result = completion_result.expect("completion task");
    assert!(
        matches!(completion_result, Err(StoreError::RevisionConflict)),
        "completion failed: {completion_result:?}"
    );
    assert_cancelled(&child, "active").await;
}

#[tokio::test]
async fn model_transition_locks_run_before_frame() {
    let application_name = format!("kf-model-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;

    assert_transition_locks_run_before_frame(&child, |store, frame_id| async move {
        store
            .begin_model_attempt(frame_id, 0, json!({"prompt": "child"}))
            .await
            .map(|_| ())
    })
    .await;
}

#[tokio::test]
async fn tool_completion_locks_run_before_frame() {
    let application_name = format!("kf-tool-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let attempt = child
        .store
        .begin_model_attempt(child.child_id.clone(), 0, json!({"prompt": "child"}))
        .await
        .expect("begin model attempt");
    let tool_call = child
        .store
        .begin_tool_call(
            child.child_id.clone(),
            attempt.revision,
            "provider-call".to_owned(),
            "docs/search".to_owned(),
            child.descriptor_digest.clone(),
            json!({}),
            format!("tool-{}", Uuid::now_v7()),
        )
        .await
        .expect("begin tool call");

    assert_transition_locks_run_before_frame(&child, move |store, frame_id| async move {
        store
            .finish_tool_call(frame_id, tool_call.id, tool_call.revision, Ok(json!({})))
            .await
    })
    .await;
}

#[tokio::test]
async fn permission_resolution_locks_run_before_frame() {
    let application_name = format!("kf-permission-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let attempt = child
        .store
        .begin_model_attempt(child.child_id.clone(), 0, json!({"prompt": "child"}))
        .await
        .expect("begin model attempt");
    let tool_call = child
        .store
        .begin_tool_call(
            child.child_id.clone(),
            attempt.revision,
            "provider-call".to_owned(),
            "docs/search".to_owned(),
            child.descriptor_digest.clone(),
            json!({}),
            format!("tool-{}", Uuid::now_v7()),
        )
        .await
        .expect("begin tool call");
    let permission_id = PermissionRequestId::new(Uuid::now_v7());
    child
        .store
        .create_permission_request(
            child.child_id.clone(),
            tool_call.revision,
            NewPermissionRequest {
                id: permission_id.clone(),
                provider_call_id: tool_call.provider_call_id,
                qualified_tool: "docs/search".to_owned(),
                descriptor_digest: child.descriptor_digest.clone(),
                input_digest: kiteframe::canonical_sha256(&json!({})).expect("input digest"),
                policy_revision: PolicyRevision::new(0),
            },
        )
        .await
        .expect("create permission request");
    let permission_revision = tool_call.revision + 1;

    assert_transition_locks_run_before_frame(&child, move |store, frame_id| async move {
        store
            .resolve_permission(
                frame_id,
                permission_id,
                permission_revision,
                json!({"decision": "allow"}),
                format!("resolve-{}", Uuid::now_v7()),
            )
            .await
            .map(|_| ())
    })
    .await;
}

#[tokio::test]
async fn permission_creation_serializes_policy_revision_validation() {
    let application_name = format!("kf-policy-lock-{}", Uuid::now_v7());
    let child = child_run(&application_name).await;
    let attempt = child
        .store
        .begin_model_attempt(child.child_id.clone(), 0, json!({"prompt": "child"}))
        .await
        .expect("begin model attempt");
    let tool_call = child
        .store
        .begin_tool_call(
            child.child_id.clone(),
            attempt.revision,
            "provider-call".to_owned(),
            "docs/search".to_owned(),
            child.descriptor_digest.clone(),
            json!({}),
            format!("tool-{}", Uuid::now_v7()),
        )
        .await
        .expect("begin tool call");

    let mut tool_gate = child.store.pool().begin().await.expect("tool gate");
    sqlx::query("SELECT id FROM tool_calls WHERE id = $1 FOR UPDATE")
        .bind(tool_call.id)
        .execute(&mut *tool_gate)
        .await
        .expect("lock tool call");

    let permission_store = Arc::clone(&child.store);
    let permission_frame = child.child_id.clone();
    let permission_digest = child.descriptor_digest.clone();
    let provider_call_id = tool_call.provider_call_id;
    let mut permission = tokio::spawn(async move {
        permission_store
            .create_permission_request(
                permission_frame,
                tool_call.revision,
                NewPermissionRequest {
                    id: PermissionRequestId::new(Uuid::now_v7()),
                    provider_call_id,
                    qualified_tool: "docs/search".to_owned(),
                    descriptor_digest: permission_digest,
                    input_digest: kiteframe::canonical_sha256(&json!({})).expect("input digest"),
                    policy_revision: PolicyRevision::new(0),
                },
            )
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 1).await;

    let policy_store = Arc::clone(&child.store);
    let policy_session = child.session_id.clone();
    let mut policy = tokio::spawn(async move {
        policy_store
            .append_policy_rules(PolicyUpdate {
                session: policy_session,
                expected_revision: PolicyRevision::new(0),
                idempotency_key: format!("policy-{}", Uuid::now_v7()),
                typed_source: json!({"version": 1, "rules": []}),
                compiled_cedar: String::new(),
            })
            .await
    });
    wait_for_blocked_connections(&child.store, &application_name, 2).await;
    tool_gate.commit().await.expect("release tool gate");

    let (permission_result, policy_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(&mut permission, &mut policy)
    })
    .await
    .expect("permission creation and policy update must terminate");
    let permission_result = permission_result.expect("permission task");
    assert!(
        permission_result.is_ok(),
        "permission creation failed: {permission_result:?}"
    );
    let policy_result = policy_result.expect("policy task");
    assert!(
        policy_result.is_ok(),
        "policy update failed: {policy_result:?}"
    );
}

#[tokio::test]
async fn recovery_batches_claim_run_aggregates_before_frames() {
    let application_name = format!("kf-recovery-lock-{}", Uuid::now_v7());
    let first_run = child_run(&application_name).await;
    let second_run = child_run(&application_name).await;

    for frame_id in [
        first_run.child_id.clone(),
        first_run.sibling_id.clone(),
        second_run.child_id.clone(),
        second_run.sibling_id.clone(),
    ] {
        first_run
            .store
            .begin_model_attempt(frame_id, 0, json!({"prompt": "recover"}))
            .await
            .expect("create uncommitted model attempt");
    }

    let ordered_frames = [
        first_run.child_id.clone(),
        second_run.child_id.clone(),
        first_run.sibling_id.clone(),
        second_run.sibling_id.clone(),
    ];
    let base_time = OffsetDateTime::now_utc() - TimeDuration::minutes(1);
    for (offset, frame_id) in ordered_frames.iter().enumerate() {
        sqlx::query("UPDATE frames SET updated_at = $1 WHERE id = $2")
            .bind(base_time + TimeDuration::seconds(offset as i64))
            .bind(*frame_id.as_uuid())
            .execute(first_run.store.pool())
            .await
            .expect("order recovery frame");
    }

    let mut frame_gates = first_run.store.pool().begin().await.expect("frame gates");
    let gated_frames = vec![
        *second_run.child_id.as_uuid(),
        *first_run.sibling_id.as_uuid(),
    ];
    sqlx::query("SELECT id FROM frames WHERE id = ANY($1) ORDER BY id FOR UPDATE")
        .bind(&gated_frames)
        .fetch_all(&mut *frame_gates)
        .await
        .expect("gate interleaved frames");
    let mut run_gate = first_run.store.pool().begin().await.expect("run gate");
    sqlx::query("SELECT id FROM runs WHERE id = $1 FOR UPDATE")
        .bind(*first_run.run_id.as_uuid())
        .execute(&mut *run_gate)
        .await
        .expect("gate first run");

    let recovery_store = Arc::clone(&first_run.store);
    let mut first_batch =
        tokio::spawn(async move { recovery_store.requeue_uncommitted_model_attempts(2).await });
    let first_batch_waited =
        wait_for_blocked_or_finished(&first_run.store, &application_name, &first_batch).await;

    if first_batch_waited {
        // The former frame-first implementation owns A/D here and waits for
        // run 1. Releasing B/C lets a second batch own run 2 and wait for run 1,
        // reproducing the opposing run-order cycle.
        frame_gates.commit().await.expect("release frame gates");
        let recovery_store = Arc::clone(&first_run.store);
        let mut second_batch =
            tokio::spawn(async move { recovery_store.requeue_uncommitted_model_attempts(2).await });
        wait_for_blocked_connections(&first_run.store, &application_name, 2).await;
        run_gate.commit().await.expect("release run gate");
        let (first_result, second_result) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(&mut first_batch, &mut second_batch)
        })
        .await
        .expect("concurrent recovery batches must terminate");
        let first_result = first_result.expect("first recovery task");
        let second_result = second_result.expect("second recovery task");
        assert!(first_result.is_ok(), "first batch failed: {first_result:?}");
        assert!(
            second_result.is_ok(),
            "second batch failed: {second_result:?}"
        );
    } else {
        frame_gates.commit().await.expect("release frame gates");
        run_gate.commit().await.expect("release run gate");
        let mut recovered = first_batch
            .await
            .expect("first recovery task")
            .expect("first recovery batch");
        loop {
            let count = first_run
                .store
                .requeue_uncommitted_model_attempts(2)
                .await
                .expect("drain recovery batch");
            recovered += count;
            if count == 0 {
                break;
            }
        }
        assert!(
            recovered >= 4,
            "the global worker must recover every fixture frame"
        );
    }

    let states =
        sqlx::query_scalar::<_, String>("SELECT state FROM frames WHERE id = ANY($1) ORDER BY id")
            .bind(
                ordered_frames
                    .iter()
                    .map(|frame| *frame.as_uuid())
                    .collect::<Vec<_>>(),
            )
            .fetch_all(first_run.store.pool())
            .await
            .expect("recovered frame states");
    assert_eq!(states.len(), 4);
    assert!(states.iter().all(|state| state == "runnable"));
}
