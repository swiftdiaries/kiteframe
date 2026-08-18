use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kiteframe::dsl::{DefinitionKind, Limits, compile};
use kiteframe::{
    AgentKernel, AgentSelector, CatalogTool, ClosedSchema, CreateSession, EffectKind, FrameCommand,
    GoalContract, IdempotencyKey, KiteAgentRuntime, NewChild, NewPermissionRequest,
    PermissionRequestId, PolicyRevision, PostgresStore, PrincipalId, PublishedDefinition, Question,
    RecoverySupport, RuntimeCeilings, SessionView, StartAgent, StartRun, StoreError, StoreLimits,
    SubmitUserTask, SuccessCriterion, TaskOwnerAuthorizer, TaskPreferences, TaskTarget,
    ToolCatalogSnapshot, UserTaskId,
};
use serde_json::json;
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

async fn agent_goal_fingerprint(store: &PostgresStore, task_id: &UserTaskId) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object( \
             'definition', (SELECT to_jsonb(d) FROM workflow_definitions d \
                 JOIN user_tasks t ON t.definition_id = d.id WHERE t.id = $1), \
             'task', (SELECT to_jsonb(t) FROM user_tasks t WHERE t.id = $1), \
             'goalContract', (SELECT to_jsonb(g) FROM goal_contracts g WHERE g.task_id = $1), \
             'goalState', (SELECT to_jsonb(g) FROM goal_states g WHERE g.task_id = $1), \
             'snapshots', (SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY s.task_id), '[]'::jsonb) \
                 FROM agent_runtime_snapshots s WHERE s.task_id = $1), \
             'transitions', (SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY t.id), '[]'::jsonb) \
                 FROM agent_transitions t WHERE t.task_id = $1), \
             'commands', (SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.id), '[]'::jsonb) \
                 FROM agent_commands c WHERE c.task_id = $1), \
             'policies', (SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.id), '[]'::jsonb) \
                 FROM agent_policy_decisions p WHERE p.task_id = $1), \
             'evidence', (SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.id), '[]'::jsonb) \
                 FROM goal_evidence e WHERE e.task_id = $1), \
             'assertions', (SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.criterion_id), '[]'::jsonb) \
                 FROM goal_criterion_assertions a WHERE a.task_id = $1), \
             'blockers', (SELECT COALESCE(jsonb_agg(to_jsonb(b) ORDER BY b.id), '[]'::jsonb) \
                 FROM goal_blockers b WHERE b.task_id = $1), \
             'actions', (SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY a.id), '[]'::jsonb) \
                 FROM agent_task_actions a WHERE a.task_id = $1), \
             'events', (SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.sequence), '[]'::jsonb) \
                 FROM agent_task_events e WHERE e.task_id = $1))",
    )
    .bind(task_id.as_uuid())
    .fetch_one(store.pool())
    .await
    .expect("fingerprint agent and goal state")
}

#[tokio::test]
async fn start_run_snapshots_definition_and_replays_only_identical_requests() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping execution store test: DATABASE_URL is not configured");
        return;
    };
    let store = Arc::new(
        PostgresStore::connect(&database_url)
            .await
            .expect("connect store"),
    );
    let principal = PrincipalId::new(format!("alice-{}", Uuid::now_v7())).unwrap();
    let session = store
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("create session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [tool("docs/search")])
            .expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("append catalog");

    let agent_name = format!("execution_store_agent_{}", Uuid::now_v7().simple());
    let agent_source = format!(
        r#"agent {agent_name}(repository: string, pull_request: integer) -> string {{
  entry inspect(repository = repository, pull_request = pull_request)
  limits {{ steps: 2 active_execution_seconds: 10 elapsed_seconds: 20 }}
  task inspect(repository: string, pull_request: integer) -> string {{
    model reasoning
    prompt """Keep the agent state immutable during legacy work."""
    tools []
    on finished(summary: string) {{ done summary }}
  }}
}}"#
    );
    let agent_definition = compile(&agent_source, &catalog, &Limits::default()).unwrap();
    let published_agent = store
        .publish(session.id.clone(), &agent_definition, &agent_source)
        .await
        .expect("publish agent for noninterference");
    let accepted_agent = store
        .submit_user_task(
            &principal,
            SubmitUserTask {
                session_id: session.id.clone(),
                agent: AgentSelector {
                    name: published_agent.name,
                    version: Some(published_agent.version),
                },
                target: TaskTarget::GithubPullRequest {
                    repository: "acme/immutable-agent".to_owned(),
                    number: 142,
                },
                preferences: TaskPreferences { harness_role: None },
                goal: GoalContract {
                    version: 1,
                    objective: "Keep agent authority isolated from legacy execution.".to_owned(),
                    success: vec![SuccessCriterion {
                        id: "agent_unchanged".to_owned(),
                        statement: "The agent and goal fingerprint remains exact.".to_owned(),
                        evidence: "harness.invocation_result".to_owned(),
                    }],
                    constraints: vec![
                        "Do not mutate agent state from legacy execution.".to_owned(),
                    ],
                    blocked_when: vec!["The isolation invariant cannot be verified.".to_owned()],
                },
                idempotency_key: IdempotencyKey::new(format!("agent-{}", Uuid::now_v7())).unwrap(),
            },
        )
        .await
        .expect("submit agent for noninterference");
    let kernel = AgentKernel::new(
        store.clone(),
        Arc::new(KiteAgentRuntime::new(RuntimeCeilings::default())),
        Arc::new(TaskOwnerAuthorizer::new(store.clone())),
    );
    kernel
        .start_agent(
            &principal,
            StartAgent {
                session_id: session.id.clone(),
                task_id: accepted_agent.id.clone(),
                expected_task_revision: 0,
                expected_goal_revision: 0,
                idempotency_key: IdempotencyKey::new(format!("start-{}", Uuid::now_v7())).unwrap(),
            },
        )
        .await
        .expect("start agent for noninterference");

    let workflow_name = format!("w{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {workflow_name}(query: string) -> string {{ start t(query: query) }} task t(query: string) -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok() {{ done query }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("compile workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
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
    assert_eq!(
        store
            .get_run(session.id.clone(), started.id.clone())
            .await
            .expect("get run"),
        started
    );
    let events = store
        .events_after(session.id.clone(), started.id.clone(), 0, 8)
        .await
        .expect("replay events");
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(events[0].kind, "run.started");
    let runnable = store
        .runnable_frames_for_run(started.id.clone(), 8)
        .await
        .expect("list this run's runnable frames");
    assert!(
        runnable
            .iter()
            .any(|frame| frame.id == started.root_frame_id)
    );
    assert_eq!(
        store.start_run(request.clone()).await.expect("replay"),
        started
    );
    let attempt = store
        .begin_model_attempt(started.root_frame_id.clone(), 0, json!({"prompt": "x"}))
        .await
        .expect("persist model intent");
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.revision, 1);
    let event_count_before_stale = store
        .events_after(session.id.clone(), started.id.clone(), 0, 8)
        .await
        .expect("events before stale completion")
        .len();
    assert!(matches!(
        store
            .commit_model_attempt(
                started.root_frame_id.clone(),
                attempt.attempt,
                attempt.revision - 1,
                vec![json!({"type": "stale"})],
            )
            .await,
        Err(StoreError::RevisionConflict)
    ));
    assert_eq!(
        store
            .events_after(session.id.clone(), started.id.clone(), 0, 8)
            .await
            .expect("stale completion must not append an event")
            .len(),
        event_count_before_stale
    );
    assert_eq!(
        store
            .model_attempt_count(started.root_frame_id.clone())
            .await
            .expect("count attempts"),
        1
    );
    store
        .commit_model_attempt(
            started.root_frame_id.clone(),
            attempt.attempt,
            attempt.revision,
            vec![json!({"type": "reasoning"}), json!({"type": "tool_call"})],
        )
        .await
        .expect("persist ordered model result");
    let tool_call = store
        .begin_tool_call(
            started.root_frame_id.clone(),
            2,
            "provider-call-1".to_owned(),
            "docs/search".to_owned(),
            catalog.tools()["docs/search"]
                .descriptor_digest()
                .to_owned(),
            json!({"query": "durable"}),
            "tool-attempt-1".to_owned(),
        )
        .await
        .expect("persist tool intent");
    assert_eq!(tool_call.revision, 3);
    assert_eq!(
        store
            .begin_tool_call(
                started.root_frame_id.clone(),
                2,
                "provider-call-1".to_owned(),
                "docs/search".to_owned(),
                catalog.tools()["docs/search"]
                    .descriptor_digest()
                    .to_owned(),
                json!({"query": "durable"}),
                "tool-attempt-1".to_owned(),
            )
            .await
            .expect("replay tool intent"),
        tool_call
    );
    let event_count_before_conflict = store
        .events_after(session.id.clone(), started.id.clone(), 0, 16)
        .await
        .expect("events before conflicting tool replay")
        .len();
    assert!(matches!(
        store
            .begin_tool_call(
                started.root_frame_id.clone(),
                2,
                "provider-call-1".to_owned(),
                "docs/search".to_owned(),
                catalog.tools()["docs/search"]
                    .descriptor_digest()
                    .to_owned(),
                json!({"query": "changed"}),
                "tool-attempt-1".to_owned(),
            )
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(
        store
            .events_after(session.id.clone(), started.id.clone(), 0, 16)
            .await
            .expect("conflicting tool replay must not append an event")
            .len(),
        event_count_before_conflict
    );
    let permission = NewPermissionRequest {
        id: PermissionRequestId::new(Uuid::now_v7()),
        provider_call_id: tool_call.provider_call_id.clone(),
        qualified_tool: "docs/search".to_owned(),
        descriptor_digest: catalog.tools()["docs/search"]
            .descriptor_digest()
            .to_owned(),
        input_digest: kiteframe::canonical_sha256(&json!({"query": "durable"})).unwrap(),
        policy_revision: PolicyRevision::new(0),
    };
    store
        .create_permission_request(
            started.root_frame_id.clone(),
            tool_call.revision,
            permission.clone(),
        )
        .await
        .expect("persist permission request");
    store
        .resolve_permission(
            started.root_frame_id.clone(),
            permission.id,
            4,
            json!({"decision": "allow"}),
            "approve-1".to_owned(),
        )
        .await
        .expect("resolve permission request");
    store
        .finish_tool_call(
            started.root_frame_id.clone(),
            tool_call.id,
            5,
            Ok(json!({"results": []})),
        )
        .await
        .expect("persist tool result");
    let question = Question {
        id: Uuid::now_v7(),
        binding: "confirmation".to_owned(),
        text: "Continue?".to_owned(),
        schema: ClosedSchema::boolean(),
    };
    store
        .create_question(started.root_frame_id.clone(), 6, question.clone())
        .await
        .expect("persist question");
    store
        .answer(
            started.root_frame_id.clone(),
            question.id,
            7,
            json!(true),
            "answer-1".to_owned(),
        )
        .await
        .expect("persist answer");
    let resumed = store
        .begin_model_attempt(
            started.root_frame_id.clone(),
            8,
            json!({"prompt": "resume"}),
        )
        .await
        .expect("resume frame");
    store
        .apply_frame_command(
            started.root_frame_id.clone(),
            resumed.revision,
            FrameCommand::StartCall(NewChild {
                target: "t".to_owned(),
                binding: "child_result".to_owned(),
                arguments: BTreeMap::from([("query".to_owned(), json!("child"))]),
                branch_index: 0,
            }),
        )
        .await
        .expect("start child frame");
    let child_id = sqlx::query("SELECT id FROM frames WHERE parent_frame_id = $1")
        .bind(*started.root_frame_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .expect("find child")
        .get("id");
    let child = kiteframe::FrameId::new(child_id);
    let child_attempt = store
        .begin_model_attempt(child.clone(), 0, json!({"prompt": "child"}))
        .await
        .expect("start child");
    store
        .apply_frame_command(
            child,
            child_attempt.revision,
            FrameCommand::Complete(json!("child done")),
        )
        .await
        .expect("complete child");
    let final_attempt = store
        .begin_model_attempt(
            started.root_frame_id.clone(),
            11,
            json!({"prompt": "finish"}),
        )
        .await
        .expect("resume parent");
    let agent_before = agent_goal_fingerprint(&store, &accepted_agent.id).await;
    store
        .apply_frame_command(
            started.root_frame_id.clone(),
            final_attempt.revision,
            FrameCommand::Complete(json!("done")),
        )
        .await
        .expect("complete root frame");
    assert_eq!(
        agent_goal_fingerprint(&store, &accepted_agent.id).await,
        agent_before
    );
    let cancellable = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({"query": "cancel"}),
            idempotency_key: IdempotencyKey::new(format!("cancel-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .expect("start cancellable run");
    let late_attempt = store
        .begin_model_attempt(
            cancellable.root_frame_id.clone(),
            0,
            json!({"prompt": "cancelled while waiting"}),
        )
        .await
        .expect("persist model intent before cancellation");
    let late_tool = store
        .begin_tool_call(
            cancellable.root_frame_id.clone(),
            late_attempt.revision,
            "cancelled-tool".to_owned(),
            "docs/search".to_owned(),
            catalog.tools()["docs/search"]
                .descriptor_digest()
                .to_owned(),
            json!({"query": "late"}),
            "cancelled-tool-key".to_owned(),
        )
        .await
        .expect("persist tool intent before cancellation");
    store
        .cancel_run(
            cancellable.id.clone(),
            0,
            Some("operator request".to_owned()),
            "cancel-1".to_owned(),
        )
        .await
        .expect("cancel run");
    store
        .cancel_run(
            cancellable.id.clone(),
            0,
            Some("operator request".to_owned()),
            "cancel-1".to_owned(),
        )
        .await
        .expect("replay cancel");
    assert!(
        !store
            .commit_model_attempt(
                cancellable.root_frame_id.clone(),
                late_attempt.attempt,
                late_attempt.revision,
                vec![json!({"type": "late_model_result"})],
            )
            .await
            .expect("late model result is recorded without reviving the cancelled run")
    );
    let cancelled = store
        .get_run(session.id.clone(), cancellable.id.clone())
        .await
        .expect("cancelled run");
    assert_eq!(cancelled.state, kiteframe::RunState::Cancelled);
    let late_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM model_items WHERE frame_id = $1 AND attempt = $2 AND direction = 'output'",
    )
    .bind(*cancellable.root_frame_id.as_uuid())
    .bind(late_attempt.attempt as i32)
    .fetch_one(store.pool())
    .await
    .expect("count late model items");
    assert_eq!(late_items, 1);
    store
        .finish_tool_call(
            cancellable.root_frame_id.clone(),
            late_tool.id,
            late_tool.revision,
            Ok(json!({"result": "late external effect"})),
        )
        .await
        .expect("late tool result is recorded without reviving the cancelled run");
    let late_tool_state: String = sqlx::query_scalar("SELECT state FROM tool_calls WHERE id = $1")
        .bind(late_tool.id)
        .fetch_one(store.pool())
        .await
        .expect("late tool state");
    assert_eq!(late_tool_state, "completed");
    assert!(
        store
            .events_after(session.id.clone(), cancellable.id, 0, 16)
            .await
            .expect("cancelled run events")
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>()
            .windows(2)
            .any(|kinds| kinds == ["model.late_result", "tool.late_result"])
    );
    let recoverable = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({"query": "recover"}),
            idempotency_key: IdempotencyKey::new(format!("recover-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .expect("start recoverable run");
    store
        .begin_model_attempt(
            recoverable.root_frame_id.clone(),
            0,
            json!({"prompt":"recover"}),
        )
        .await
        .expect("interrupted model intent");
    assert!(
        store
            .requeue_uncommitted_model_attempts(256)
            .await
            .expect("recover model intent")
            >= 1
    );
    assert!(
        store
            .runnable_frames_for_run(recoverable.id.clone(), 8)
            .await
            .expect("requeued frame")
            .iter()
            .any(|frame| frame.id == recoverable.root_frame_id)
    );
    let completable = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({"query": "complete"}),
            idempotency_key: IdempotencyKey::new(format!("complete-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .expect("start completable run");
    let completion_attempt = store
        .begin_model_attempt(
            completable.root_frame_id.clone(),
            0,
            json!({"prompt": "complete"}),
        )
        .await
        .expect("completion intent");
    store
        .commit_model_attempt(
            completable.root_frame_id.clone(),
            completion_attempt.attempt,
            completion_attempt.revision,
            vec![json!({"type": "function_call"})],
        )
        .await
        .expect("completion output");
    store
        .record_completion(
            completable.root_frame_id.clone(),
            2,
            "ok".to_owned(),
            json!({}),
        )
        .await
        .expect("record completion");
    assert!(
        store
            .runnable_frames_for_run(completable.id.clone(), 8)
            .await
            .expect("this run's runnable frames")
            .iter()
            .any(|frame| frame.id == completable.root_frame_id
                && frame.outcome_name.as_deref() == Some("ok"))
    );
    assert!(matches!(
        store
            .start_run(StartRun {
                input: json!({"query": "changed"}),
                ..StartRun {
                    session_id: session.id,
                    workflow_name: definition.name,
                    version: Some(definition.version),
                    input: json!({"query": "ignored"}),
                    idempotency_key: key,
                }
            })
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
}

#[tokio::test]
async fn active_run_quota_rejects_new_work_but_replays_the_original_request() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = PostgresStore::connect_with_limits(
        &database_url,
        StoreLimits {
            max_active_runs_per_session: 1,
            ..StoreLimits::default()
        },
    )
    .await
    .expect("quota store");
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("run-quota-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/quota".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let name = format!("quota{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"ok\" }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let request = StartRun {
        session_id: session.id.clone(),
        workflow_name: definition.name.clone(),
        version: Some(definition.version),
        input: json!({}),
        idempotency_key: IdempotencyKey::new(format!("quota-start-{}", Uuid::now_v7())).unwrap(),
    };
    let first = store.start_run(request.clone()).await.expect("first run");
    assert_eq!(
        store.start_run(request).await.expect("idempotent replay"),
        first
    );
    assert!(matches!(
        store
            .start_run(StartRun {
                session_id: session.id,
                workflow_name: definition.name,
                version: Some(definition.version),
                input: json!({}),
                idempotency_key: IdempotencyKey::new(format!("quota-start-{}", Uuid::now_v7()))
                    .unwrap(),
            })
            .await,
        Err(StoreError::QuotaExceeded("active_runs_per_session"))
    ));
}

#[tokio::test]
async fn concurrent_active_run_quota_admits_exactly_one_distinct_request() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = std::sync::Arc::new(
        PostgresStore::connect_with_limits(
            &database_url,
            StoreLimits {
                max_active_runs_per_session: 1,
                ..StoreLimits::default()
            },
        )
        .await
        .expect("quota store"),
    );
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("run-quota-race-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/quota".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let name = format!("quotarace{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"ok\" }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let make_request = || StartRun {
        session_id: session.id.clone(),
        workflow_name: definition.name.clone(),
        version: Some(definition.version),
        input: json!({}),
        idempotency_key: IdempotencyKey::new(format!("quota-race-{}", Uuid::now_v7())).unwrap(),
    };
    let (first, second) = tokio::join!(
        store.start_run(make_request()),
        store.start_run(make_request())
    );
    match (first, second) {
        (Ok(_), Err(StoreError::QuotaExceeded("active_runs_per_session")))
        | (Err(StoreError::QuotaExceeded("active_runs_per_session")), Ok(_)) => {}
        results => panic!("expected one creation and one quota rejection, got {results:?}"),
    }
}

async fn execution_fixture_with_agent() -> Option<(
    PostgresStore,
    sqlx::PgPool,
    SessionView,
    PublishedDefinition,
)> {
    let database_url = std::env::var("DATABASE_URL").ok()?;
    let store = PostgresStore::connect_with_limits(
        &database_url,
        StoreLimits {
            max_active_runs_per_session: 0,
            ..StoreLimits::default()
        },
    )
    .await
    .expect("agent boundary store");
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("agent-start-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/agent-start".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("agent boundary session");
    let catalog = store
        .current_catalog(session.id.clone())
        .await
        .expect("initial catalog");
    let name = format!("agentstart_{}", Uuid::now_v7().simple());
    let source = format!(
        r#"agent {name}() -> string {{
  entry inspect()
  events ["test.agent.changed.v1"]
  limits {{ steps: 5 active_execution_seconds: 10 elapsed_seconds: 20 }}
  task inspect() -> string {{
    model reasoning
    prompt """Inspect without running."""
    tools []
    on finished(summary: string) {{ done summary }}
  }}
}}"#
    );
    let compiled = compile(&source, &catalog, &Limits::default()).expect("agent compiles");
    assert_eq!(compiled.definition_kind, DefinitionKind::Agent);
    let agent = store
        .publish(session.id.clone(), &compiled, &source)
        .await
        .expect("publish agent");
    let pool = store.pool().clone();
    Some((store, pool, session, agent))
}

#[tokio::test]
async fn legacy_run_start_rejects_agent_without_execution_rows() {
    let Some((store, pool, session, agent)) = execution_fixture_with_agent().await else {
        eprintln!("skipping agent execution-boundary test: DATABASE_URL is not configured");
        return;
    };
    let error = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: agent.name,
            version: Some(agent.version),
            input: json!({}),
            idempotency_key: IdempotencyKey::new("must-not-run-agent").unwrap(),
        })
        .await
        .expect_err("agents are inert in the foundation slice");
    assert!(matches!(error, StoreError::AgentRuntimeUnavailable));

    for (table, query) in [
        ("runs", "SELECT COUNT(*) FROM runs WHERE session_id = $1"),
        (
            "frames",
            "SELECT COUNT(*) FROM frames f JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
        ),
        (
            "events",
            "SELECT COUNT(*) FROM events e JOIN runs r ON r.id = e.run_id WHERE r.session_id = $1",
        ),
        (
            "run_tool_bindings",
            "SELECT COUNT(*) FROM run_tool_bindings b JOIN runs r ON r.id = b.run_id WHERE r.session_id = $1",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .bind(*session.id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "unexpected row in {table}");
    }
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
