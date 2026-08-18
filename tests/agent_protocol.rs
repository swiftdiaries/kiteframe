use async_trait::async_trait;
#[cfg(feature = "acp-v2")]
use kiteframe::V2Adapter;
use kiteframe::dsl::{DefinitionKind, Limits, compile};
use kiteframe::{
    AcceptedUserTask, AgentExecutionView, AgentSelector, Application, ApplicationCommand,
    ApplicationContext, ApplicationResult, CreateSession, Engine, EngineConfig, GoalBlocker,
    GoalContract, GoalPhase, HarnessRole, IdempotencyKey, InProcessToolRegistry, ModelClient,
    ModelError, ModelRequest, ModelResponse, OperationalStatus, PauseUserTask, PersistedAuthorizer,
    PostgresStore, ProtocolAdapter, PublishedDefinition, ResumeUserTask, RuntimeCeilings,
    RuntimeDisposition, RuntimeStateSnapshot, SessionId, StartAgent, StoreError, SubmitUserTask,
    SuccessCriterion, TaskLifecycle, TaskPreferences, TaskTarget, UserTaskId, V1Adapter,
};
use serde_json::json;
use std::sync::Arc;
use time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

const SESSION: &str = "018f0f47-5d8b-7d5a-9cb1-3a09d0d4c902";
const TASK: &str = "018f0f47-5d8b-7d5a-9cb1-3a09d0d4c903";

fn command_tag(command: &ApplicationCommand) -> &'static str {
    match command {
        ApplicationCommand::StartAgent(_) => "agent_start",
        ApplicationCommand::GetAgentExecution(_) => "agent_get",
        ApplicationCommand::PauseTask(_) => "task_pause",
        ApplicationCommand::ResumeTask(_) => "task_resume",
        ApplicationCommand::CancelTask(_) => "task_cancel",
        ApplicationCommand::ReplayAgentEvents(_) => "task_events",
        _ => "legacy",
    }
}

#[test]
#[cfg(feature = "acp-v2")]
fn every_agent_lifecycle_method_decodes_identically_in_v1_and_v2() {
    let cases = [
        (
            "_kiteframe/agent/start",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 0, "expectedGoalRevision": 0,
                "idempotencyKey": "start-agent"
            }),
        ),
        (
            "_kiteframe/agent/get",
            json!({"sessionId": SESSION, "taskId": TASK}),
        ),
        (
            "_kiteframe/task/pause",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 1, "expectedGoalRevision": 0,
                "reason": "User paused work.", "idempotencyKey": "pause-agent"
            }),
        ),
        (
            "_kiteframe/task/resume",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 2, "expectedGoalRevision": 1,
                "idempotencyKey": "resume-agent"
            }),
        ),
        (
            "_kiteframe/task/cancel",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 3, "expectedGoalRevision": 2,
                "reason": "No longer needed.", "idempotencyKey": "cancel-agent"
            }),
        ),
        (
            "_kiteframe/task/events",
            json!({"sessionId": SESSION, "taskId": TASK, "after": 0, "limit": 32}),
        ),
    ];

    for (method, params) in cases {
        let v1 = V1Adapter::decode_extension(method, params.clone()).unwrap();
        let v2 = V2Adapter::new(true)
            .decode_extension(method, params)
            .unwrap();
        assert_eq!(command_tag(&v1), command_tag(&v2));
    }
}

fn blocked_execution_fixture() -> AgentExecutionView {
    let task_id = UserTaskId::new(Uuid::parse_str(TASK).unwrap());
    AgentExecutionView {
        task: AcceptedUserTask {
            id: task_id.clone(),
            session_id: SessionId::new(Uuid::parse_str(SESSION).unwrap()),
            principal: kiteframe::PrincipalId::new("user_456").unwrap(),
            definition: PublishedDefinition {
                id: Uuid::parse_str("018f0f47-5d8b-7d5a-9cb1-3a09d0d4c904").unwrap(),
                kind: DefinitionKind::Agent,
                name: "babysit_pr".to_owned(),
                version: 1,
                definition_hash: "sha256".to_owned(),
            },
            target: TaskTarget::GithubPullRequest {
                repository: "acme/payments".to_owned(),
                number: 142,
            },
            preferences: TaskPreferences {
                harness_role: Some(HarnessRole::Coding),
            },
            goal: GoalContract {
                version: 1,
                objective: "Address review feedback safely.".to_owned(),
                success: vec![SuccessCriterion {
                    id: "review_verified".to_owned(),
                    statement: "The bounded review check succeeded.".to_owned(),
                    evidence: "harness.invocation_result".to_owned(),
                }],
                constraints: vec!["Do not mutate repository state.".to_owned()],
                blocked_when: vec!["Required review information is unavailable.".to_owned()],
            },
            goal_digest: "goal-sha256".to_owned(),
            revision: 0,
            created_at: OffsetDateTime::UNIX_EPOCH,
        },
        lifecycle: TaskLifecycle::Running,
        operational_status: OperationalStatus::Blocked,
        task_revision: 4,
        goal_phase: GoalPhase::Blocked,
        goal_revision: 3,
        blocker: Some(GoalBlocker {
            blocked_when_index: 0,
            reason_code: "required_information_unavailable".to_owned(),
            explanation: "A bounded explanation.".to_owned(),
            evidence_ids: vec![],
            unlock_conditions: vec!["Review information becomes available.".to_owned()],
            goal_revision: 3,
        }),
        runtime: Some(
            RuntimeStateSnapshot::new(
                RuntimeDisposition::Blocked,
                json!({"frames": []}),
                &RuntimeCeilings::default(),
            )
            .unwrap(),
        ),
    }
}

#[test]
fn agent_read_model_keeps_all_three_state_axes_distinct() {
    let encoded = V1Adapter
        .encode_result(ApplicationResult::AgentExecution(
            blocked_execution_fixture(),
        ))
        .unwrap();

    assert_eq!(encoded["lifecycle"], "running");
    assert_eq!(encoded["operationalStatus"], "blocked");
    assert_eq!(encoded["goal"]["phase"], "blocked");
    assert_eq!(encoded["goal"]["blocker"]["blockedWhenIndex"], 0);
    assert_eq!(encoded["runtime"]["runtimeId"], "kite.v1");
    assert!(encoded["runtime"].get("payload").is_none());
}

struct UnusedModel;

#[async_trait]
impl ModelClient for UnusedModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Protocol(
            "model is not called by protocol dispatch tests".to_owned(),
        ))
    }
}

async fn application_fixture() -> Option<(
    Application,
    Arc<PostgresStore>,
    ApplicationContext,
    AcceptedUserTask,
)> {
    let database_url = std::env::var("DATABASE_URL").ok()?;
    let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let principal =
        kiteframe::PrincipalId::new(format!("agent-protocol-{}", Uuid::now_v7())).unwrap();
    let session = store
        .create_session(CreateSession {
            principal: principal.clone(),
            working_directory: "/agent-protocol".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let catalog = store.current_catalog(session.id.clone()).await.unwrap();
    let name = format!("protocol_agent_{}", Uuid::now_v7().simple());
    let source = format!(
        r#"agent {name}(repository: string, pull_request: integer) -> string {{
  entry inspect(repository = repository, pull_request = pull_request)
  limits {{ active_execution_seconds: 60 elapsed_seconds: 600 }}
  task inspect(repository: string, pull_request: integer) -> string {{
    model reasoning
    prompt """Inspect one pull request."""
    tools []
    on inspected(summary: string) {{ done summary }}
  }}
}}"#
    );
    let definition = compile(&source, &catalog, &Limits::default()).unwrap();
    let published = store
        .publish(session.id.clone(), &definition, &source)
        .await
        .unwrap();
    let accepted = store
        .submit_user_task(
            &principal,
            SubmitUserTask {
                session_id: session.id.clone(),
                agent: AgentSelector {
                    name: published.name,
                    version: Some(published.version),
                },
                target: TaskTarget::GithubPullRequest {
                    repository: "acme/payments".to_owned(),
                    number: 142,
                },
                preferences: TaskPreferences {
                    harness_role: Some(HarnessRole::Reasoning),
                },
                goal: GoalContract {
                    version: 1,
                    objective: "Inspect the pull request safely.".to_owned(),
                    success: vec![SuccessCriterion {
                        id: "inspection_complete".to_owned(),
                        statement: "The bounded inspection succeeded.".to_owned(),
                        evidence: "harness.invocation_result".to_owned(),
                    }],
                    constraints: vec!["Do not mutate repository state.".to_owned()],
                    blocked_when: vec!["Required context is unavailable.".to_owned()],
                },
                idempotency_key: IdempotencyKey::new(format!(
                    "submit-agent-protocol-{}",
                    Uuid::now_v7()
                ))
                .unwrap(),
            },
        )
        .await
        .unwrap();
    let engine = Arc::new(Engine {
        store: store.clone(),
        model: Arc::new(UnusedModel),
        tools: Arc::new(InProcessToolRegistry::new()),
        authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
        config: EngineConfig::default(),
    });
    let application = Application::new(store.clone(), engine);
    let context = ApplicationContext {
        connection_principal: principal,
        session_id: Some(session.id),
    };
    Some((application, store, context, accepted))
}

#[tokio::test]
async fn application_dispatches_agent_lifecycle_through_one_shared_kernel() {
    let Some((application, store, context, accepted)) = application_fixture().await else {
        return;
    };
    let first_kernel = application.agent_kernel();
    let second_kernel = application.agent_kernel();
    assert!(Arc::ptr_eq(&first_kernel, &second_kernel));

    let started = application
        .dispatch(
            &context,
            ApplicationCommand::StartAgent(StartAgent {
                session_id: accepted.session_id.clone(),
                task_id: accepted.id.clone(),
                expected_task_revision: 0,
                expected_goal_revision: 0,
                idempotency_key: IdempotencyKey::new("start-agent-protocol").unwrap(),
            }),
        )
        .await
        .unwrap();
    let ApplicationResult::AgentExecution(started) = started else {
        panic!("expected agent execution");
    };
    assert_eq!(started.lifecycle, TaskLifecycle::Running);

    for (table, query) in [
        ("runs", "SELECT COUNT(*) FROM runs WHERE session_id = $1"),
        (
            "frames",
            "SELECT COUNT(*) FROM frames f JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
        ),
        (
            "model_items",
            "SELECT COUNT(*) FROM model_items m JOIN frames f ON f.id = m.frame_id JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
        ),
        (
            "tool_calls",
            "SELECT COUNT(*) FROM tool_calls t JOIN frames f ON f.id = t.frame_id JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1",
        ),
        (
            "questions",
            "SELECT COUNT(*) FROM questions q JOIN runs r ON r.id = q.run_id WHERE r.session_id = $1",
        ),
        (
            "permission_requests",
            "SELECT COUNT(*) FROM permission_requests p JOIN runs r ON r.id = p.run_id WHERE r.session_id = $1",
        ),
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .bind(accepted.session_id.as_uuid())
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "agent start wrote legacy {table} rows");
    }

    let current = application
        .dispatch(
            &context,
            ApplicationCommand::GetAgentExecution(kiteframe::GetAgentExecution {
                session_id: accepted.session_id,
                task_id: accepted.id,
            }),
        )
        .await
        .unwrap();
    let ApplicationResult::AgentExecution(current) = current else {
        panic!("expected current agent execution");
    };
    assert_eq!(current, started);
}

#[tokio::test]
async fn lifecycle_actions_and_event_replay_are_connection_scoped_and_bounded() {
    let Some((application, store, context, accepted)) = application_fixture().await else {
        return;
    };
    application
        .dispatch(
            &context,
            ApplicationCommand::StartAgent(StartAgent {
                session_id: accepted.session_id.clone(),
                task_id: accepted.id.clone(),
                expected_task_revision: 0,
                expected_goal_revision: 0,
                idempotency_key: IdempotencyKey::new("start-actions").unwrap(),
            }),
        )
        .await
        .unwrap();
    let paused = application
        .dispatch(
            &context,
            ApplicationCommand::PauseTask(PauseUserTask {
                session_id: accepted.session_id.clone(),
                task_id: accepted.id.clone(),
                expected_task_revision: 1,
                expected_goal_revision: 0,
                reason: "User paused work.".to_owned(),
                idempotency_key: IdempotencyKey::new("pause-actions").unwrap(),
            }),
        )
        .await
        .unwrap();
    let ApplicationResult::AgentExecution(paused) = paused else {
        panic!("expected paused execution");
    };
    assert_eq!(paused.operational_status, OperationalStatus::Paused);
    assert_eq!(paused.goal_phase, GoalPhase::Paused);

    let resumed = application
        .dispatch(
            &context,
            ApplicationCommand::ResumeTask(ResumeUserTask {
                session_id: accepted.session_id.clone(),
                task_id: accepted.id.clone(),
                expected_task_revision: 2,
                expected_goal_revision: 1,
                idempotency_key: IdempotencyKey::new("resume-actions").unwrap(),
            }),
        )
        .await
        .unwrap();
    let ApplicationResult::AgentExecution(resumed) = resumed else {
        panic!("expected resumed execution");
    };
    assert_eq!(
        resumed.operational_status,
        OperationalStatus::WaitingForHarness
    );
    assert_eq!(resumed.goal_phase, GoalPhase::Active);

    let cancelled = application
        .dispatch(
            &context,
            ApplicationCommand::CancelTask(kiteframe::CancelUserTask {
                session_id: accepted.session_id.clone(),
                task_id: accepted.id.clone(),
                expected_task_revision: 3,
                expected_goal_revision: 2,
                reason: Some("No longer needed.".to_owned()),
                idempotency_key: IdempotencyKey::new("cancel-actions").unwrap(),
            }),
        )
        .await
        .unwrap();
    let ApplicationResult::AgentExecution(cancelled) = cancelled else {
        panic!("expected cancelled execution");
    };
    assert_eq!(cancelled.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(cancelled.goal_phase, GoalPhase::Active);

    let replay = application
        .dispatch(
            &context,
            ApplicationCommand::ReplayAgentEvents(kiteframe::AgentEventCursor {
                session_id: accepted.session_id.clone(),
                task_id: accepted.id.clone(),
                after: 0,
                limit: 1_000,
            }),
        )
        .await
        .unwrap();
    let ApplicationResult::AgentEvents(events) = replay else {
        panic!("expected agent events");
    };
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        (1..=events.len() as u64).collect::<Vec<_>>()
    );
    assert!(events.len() >= 4);

    for (after, limit) in [(0, 0), (0, 1_001), (-1, 1)] {
        let error = application
            .dispatch(
                &context,
                ApplicationCommand::ReplayAgentEvents(kiteframe::AgentEventCursor {
                    session_id: accepted.session_id.clone(),
                    task_id: accepted.id.clone(),
                    after,
                    limit,
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, kiteframe::ProtocolError::InvalidPayload(_)));
    }

    let foreign = ApplicationContext {
        connection_principal: kiteframe::PrincipalId::new(format!(
            "foreign-agent-protocol-{}",
            Uuid::now_v7()
        ))
        .unwrap(),
        session_id: Some(accepted.session_id.clone()),
    };
    assert!(
        application
            .dispatch(
                &foreign,
                ApplicationCommand::GetAgentExecution(kiteframe::GetAgentExecution {
                    session_id: accepted.session_id.clone(),
                    task_id: accepted.id.clone(),
                }),
            )
            .await
            .is_err()
    );

    let another = store
        .create_session(CreateSession {
            principal: context.connection_principal.clone(),
            working_directory: "/agent-protocol-other".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let wrong_connection = ApplicationContext {
        connection_principal: context.connection_principal,
        session_id: Some(another.id),
    };
    assert!(matches!(
        application
            .dispatch(
                &wrong_connection,
                ApplicationCommand::GetAgentExecution(kiteframe::GetAgentExecution {
                    session_id: accepted.session_id,
                    task_id: accepted.id,
                }),
            )
            .await,
        Err(kiteframe::ProtocolError::SessionMismatch)
    ));
}

#[test]
#[cfg(feature = "acp-v2")]
fn lifecycle_wire_objects_reject_unknown_fields_and_wire_principals() {
    for (method, payload) in [
        (
            "_kiteframe/agent/start",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 0, "expectedGoalRevision": 0,
                "idempotencyKey": "start", "unknown": true
            }),
        ),
        (
            "_kiteframe/agent/get",
            json!({"sessionId": SESSION, "taskId": TASK, "principal": "attacker"}),
        ),
        (
            "_kiteframe/task/pause",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 1, "expectedGoalRevision": 0,
                "reason": "pause", "idempotencyKey": "pause", "unknown": true
            }),
        ),
        (
            "_kiteframe/task/resume",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 2, "expectedGoalRevision": 1,
                "idempotencyKey": "resume", "principalId": "attacker"
            }),
        ),
        (
            "_kiteframe/task/cancel",
            json!({
                "sessionId": SESSION, "taskId": TASK,
                "expectedTaskRevision": 3, "expectedGoalRevision": 2,
                "reason": "cancel", "idempotencyKey": "cancel", "unknown": true
            }),
        ),
        (
            "_kiteframe/task/events",
            json!({"sessionId": SESSION, "taskId": TASK, "after": 0, "limit": 32, "unknown": true}),
        ),
    ] {
        assert!(matches!(
            V1Adapter::decode_extension(method, payload.clone()),
            Err(kiteframe::ProtocolError::InvalidPayload(_))
        ));
        assert!(matches!(
            V2Adapter::new(true).decode_extension(method, payload),
            Err(kiteframe::ProtocolError::InvalidPayload(_))
        ));
    }
}

#[test]
fn lifecycle_store_failures_map_to_fixed_coarse_codes() {
    for (error, code) in [
        (
            StoreError::AgentTransitionInvalid("private transition".to_owned()),
            "agent_transition_invalid",
        ),
        (
            StoreError::RuntimeStateInvalid("private snapshot".to_owned()),
            "agent_transition_invalid",
        ),
        (StoreError::TaskRevisionConflict, "revision_conflict"),
        (StoreError::GoalRevisionConflict, "revision_conflict"),
        (StoreError::GoalAlreadyTerminal, "task_transition_invalid"),
        (
            StoreError::AgentTaskAlreadyTerminal,
            "task_transition_invalid",
        ),
        (StoreError::GoalEvidenceIncomplete, "goal_evidence_invalid"),
        (StoreError::GoalEvidenceNotFound, "goal_evidence_invalid"),
        (
            StoreError::GoalEvidenceTypeMismatch,
            "goal_evidence_invalid",
        ),
        (
            StoreError::GoalEvidenceUnauthorized,
            "goal_evidence_invalid",
        ),
        (StoreError::GoalEvidenceStale, "goal_evidence_invalid"),
    ] {
        let protocol_error = error.into();
        let encoded = V1Adapter::encode_error(&protocol_error);
        assert_coarse_code(encoded, code);
        #[cfg(feature = "acp-v2")]
        assert_coarse_code(V2Adapter::new(true).encode_error(&protocol_error), code);
    }
}

fn assert_coarse_code(error: agent_client_protocol::Error, expected: &str) {
    assert_eq!(i32::from(error.code), -32000);
    assert_eq!(error.message, "application request failed");
    assert_eq!(error.data.unwrap()["kiteframe_code"], expected);
}
