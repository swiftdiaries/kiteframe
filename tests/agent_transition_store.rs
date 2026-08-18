use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use kiteframe::dsl::{CompiledDefinition, Limits, compile};
use kiteframe::{
    AgentCommandState, AgentCommandView, AgentExecutionView, AgentKernel, AgentRuntime,
    AgentRuntimeError, AgentSelector, BlockGoal, ClaimAgentCommand, ClosedSchema,
    CompleteAgentCommand, CompleteGoal, CreateSession, EvidenceId, FailAgentCommand, FailTask,
    GoalContract, GoalPhase, IdempotencyKey, KernelCommand, KernelCommandPayload, KiteAgentRuntime,
    KiteFrameId, KiteFrameState, KiteRuntimeState, OperationalStatus, PostgresStore, PrincipalId,
    RequestApproval, RuntimeCeilings, RuntimeCommandKey, RuntimeDisposition, RuntimeEvent,
    RuntimeHandle, RuntimeHistoryItem, RuntimeHistoryKind, RuntimeStart, RuntimeStateSnapshot,
    RuntimeTransition, SessionId, SessionView, StartAgent, StoreError, SubmitUserTask,
    SuccessCriterion, TaskLifecycle, TaskOwnerAuthorizer, TaskPreferences, TaskTarget,
    ToolCatalogSnapshot, UserTaskId,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tokio::sync::Notify;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::{Layer, prelude::*};
use uuid::Uuid;

#[derive(Clone)]
struct EmptyCompletionProbeGate {
    armed: Arc<AtomicBool>,
    reached: Arc<Notify>,
    release: Arc<Barrier>,
}

impl<S> Layer<S> for EmptyCompletionProbeGate
where
    S: Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        #[derive(Default)]
        struct EventFields(String);

        impl Visit for EventFields {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                write!(&mut self.0, " {}={value:?}", field.name()).unwrap();
            }
        }

        let mut fields = EventFields::default();
        event.record(&mut fields);
        if event.metadata().target() == "sqlx::query"
            && fields
                .0
                .contains("SELECT completion_idempotency_key, completion_request_digest")
            && self.armed.swap(false, Ordering::SeqCst)
        {
            self.reached.notify_one();
            self.release.wait();
        }
    }
}

struct CountingRuntime {
    inner: KiteAgentRuntime,
    handle_calls: AtomicUsize,
}

impl CountingRuntime {
    fn new(ceilings: RuntimeCeilings) -> Self {
        Self {
            inner: KiteAgentRuntime::new(ceilings),
            handle_calls: AtomicUsize::new(0),
        }
    }
}

impl AgentRuntime for CountingRuntime {
    fn validate_agent(&self, definition: &CompiledDefinition) -> Result<(), AgentRuntimeError> {
        self.inner.validate_agent(definition)
    }

    fn start(&self, input: RuntimeStart<'_>) -> Result<RuntimeTransition, AgentRuntimeError> {
        self.inner.start(input)
    }

    fn handle(&self, input: RuntimeHandle<'_>) -> Result<RuntimeTransition, AgentRuntimeError> {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.handle(input)
    }
}

struct TransitionFixture {
    kernel: Arc<AgentKernel>,
    store: Arc<PostgresStore>,
    runtime: Arc<CountingRuntime>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    task_id: kiteframe::UserTaskId,
    ceilings: RuntimeCeilings,
}

impl TransitionFixture {
    async fn started_simple_agent() -> Option<Self> {
        Self::started_with_source(
            r#"task inspect(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Inspect.""" tools []
    on ok(answer: string) { done answer }
  }"#,
            "inspect(repository = repository, pull_request = pull_request)",
        )
        .await
    }

    async fn started_fork_agent() -> Option<Self> {
        Self::started_with_source(
            r#"task root(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Split.""" tools []
    on split(left: string, right: string) {
      fork {
        first = call branch(value = left) context isolated
        second = call branch(value = right) context kept
      } join
      done first
    }
  }
  task branch(value: string) -> string {
    model reasoning prompt """Return.""" tools []
    on ok(result: string) { done result }
  }"#,
            "root(repository = repository, pull_request = pull_request)",
        )
        .await
    }

    async fn started_with_source(tasks: &str, entry: &str) -> Option<Self> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let ceilings = RuntimeCeilings::default();
        let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let principal = PrincipalId::new(format!("agent-transition-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/agent-transition".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
        store.append_catalog_epoch(&catalog).await.unwrap();
        let name = format!("transition_agent_{}", Uuid::now_v7().simple());
        let source = format!(
            r#"agent {name}(repository: string, pull_request: integer) -> string {{
  entry {entry}
  limits {{ steps: 100 model_turns_per_task: 10 tool_calls_per_response: 4 parallel_branches: 4 response_item_bytes: 16384 active_execution_seconds: 60 elapsed_seconds: 600 }}
  {tasks}
}}"#
        );
        let definition = compile(&source, &catalog, &Limits::default()).unwrap();
        let published = store
            .publish(session.id.clone(), &definition, &definition.source)
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
                        repository: "acme/widgets".to_owned(),
                        number: 142,
                    },
                    preferences: TaskPreferences { harness_role: None },
                    goal: GoalContract {
                        version: 1,
                        objective: "Finish the task".to_owned(),
                        success: vec![SuccessCriterion {
                            id: "finished".to_owned(),
                            statement: "The task is finished".to_owned(),
                            evidence: "harness.invocation_result".to_owned(),
                        }],
                        constraints: vec![],
                        blocked_when: vec!["No safe path remains".to_owned()],
                    },
                    idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let runtime = Arc::new(CountingRuntime::new(ceilings));
        let kernel = Arc::new(AgentKernel::new(
            Arc::clone(&store),
            runtime.clone(),
            Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&store))),
        ));
        kernel
            .start_agent(
                &principal,
                StartAgent {
                    session_id: session.id.clone(),
                    task_id: accepted.id.clone(),
                    expected_task_revision: 0,
                    expected_goal_revision: 0,
                    idempotency_key: IdempotencyKey::new(format!("start-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        Some(Self {
            kernel,
            pool: store.pool().clone(),
            store,
            runtime,
            session,
            principal,
            task_id: accepted.id,
            ceilings,
        })
    }

    async fn claim_first(&self, claimant: &str, key: &str) -> AgentCommandView {
        let command = self
            .store
            .dispatchable_agent_commands(64)
            .await
            .unwrap()
            .into_iter()
            .find(|command| command.task_id == self.task_id)
            .unwrap();
        self.store
            .claim_agent_command(ClaimAgentCommand {
                session_id: self.session.id.clone(),
                task_id: self.task_id.clone(),
                command_id: command.id,
                expected_command_revision: command.revision,
                claimant: claimant.to_owned(),
                idempotency_key: IdempotencyKey::new(key).unwrap(),
            })
            .await
            .unwrap()
    }

    async fn current(&self) -> AgentExecutionView {
        self.store
            .get_agent_execution(self.session.id.clone(), self.task_id.clone())
            .await
            .unwrap()
    }

    fn harness_result(
        &self,
        command: &AgentCommandView,
        event_id: &str,
        outcome: &str,
        fields: Value,
    ) -> RuntimeEvent {
        RuntimeEvent::HarnessResult {
            event_id: event_id.to_owned(),
            command_key: command.key.clone(),
            outcome: outcome.to_owned(),
            fields,
            evidence_id: EvidenceId::new(Uuid::now_v7()),
            history: vec![],
        }
    }

    fn completion(
        &self,
        command: &AgentCommandView,
        event_id: &str,
        outcome: &str,
        fields: Value,
    ) -> CompleteAgentCommand {
        CompleteAgentCommand {
            session_id: self.session.id.clone(),
            task_id: self.task_id.clone(),
            command_id: command.id.clone(),
            expected_command_revision: command.revision,
            idempotency_key: IdempotencyKey::new(format!("complete-{event_id}")).unwrap(),
            event: self.harness_result(command, event_id, outcome, fields),
        }
    }

    async fn commands_for_latest_transition(&self) -> Vec<AgentCommandView> {
        let current = self.current().await;
        let transition_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM agent_transitions WHERE task_id = $1 AND committed_task_revision = $2",
        )
        .bind(self.task_id.as_uuid())
        .bind(i64::try_from(current.task_revision).unwrap())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM agent_commands WHERE transition_id = $1 ORDER BY ordinal",
        )
        .bind(transition_id)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        let mut commands = Vec::with_capacity(ids.len());
        for id in ids {
            commands.push(
                self.store
                    .load_agent_command_context(
                        self.session.id.clone(),
                        self.task_id.clone(),
                        kiteframe::AgentCommandId::new(id),
                    )
                    .await
                    .unwrap()
                    .command,
            );
        }
        commands
    }

    async fn transition_count_for(&self, key: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_transitions WHERE task_id = $1 AND idempotency_key = $2",
        )
        .bind(self.task_id.as_uuid())
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    fn runtime_handle_calls(&self) -> usize {
        self.runtime.handle_calls.load(Ordering::SeqCst)
    }

    async fn command_rows(&self) -> Vec<(String, String, i64, Option<String>)> {
        sqlx::query_as(
            "SELECT runtime_key, state, revision, claimant FROM agent_commands \
             WHERE task_id = $1 ORDER BY ordinal, id",
        )
        .bind(self.task_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn database_fingerprint(&self) -> Value {
        let task: (String, String, i64, Option<Value>) = sqlx::query_as(
            "SELECT state, operational_status, revision, failure FROM user_tasks WHERE id = $1",
        )
        .bind(self.task_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let goal: (String, i64) =
            sqlx::query_as("SELECT phase, revision FROM goal_states WHERE task_id = $1")
                .bind(self.task_id.as_uuid())
                .fetch_one(&self.pool)
                .await
                .unwrap();
        let snapshot: (i64, String) = sqlx::query_as(
            "SELECT task_revision, snapshot_digest FROM agent_runtime_snapshots WHERE task_id = $1",
        )
        .bind(self.task_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let commands: Value = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(jsonb_build_object( \
                 'id', id, 'key', runtime_key, 'state', state, 'revision', revision, \
                 'claimKey', claim_idempotency_key, 'completionKey', completion_idempotency_key, \
                 'result', result, 'failure', failure) ORDER BY id), '[]'::jsonb) \
             FROM agent_commands WHERE task_id = $1",
        )
        .bind(self.task_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let transitions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_transitions WHERE task_id = $1")
                .bind(self.task_id.as_uuid())
                .fetch_one(&self.pool)
                .await
                .unwrap();
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_events WHERE task_id = $1")
                .bind(self.task_id.as_uuid())
                .fetch_one(&self.pool)
                .await
                .unwrap();
        json!({
            "task": [task.0, task.1, task.2, task.3],
            "goal": [goal.0, goal.1],
            "snapshot": [snapshot.0, snapshot.1],
            "commands": commands,
            "transitions": transitions,
            "events": events,
        })
    }

    async fn reject_write(&self, name: &str, table: &str, operation: &str, when: &str) {
        let suffix = self.task_id.as_uuid().simple();
        let function_name = format!("reject_{name}_{suffix}");
        let trigger_name = format!("reject_{name}_trigger_{suffix}");
        sqlx::query(&format!(
            "CREATE FUNCTION {function_name}() RETURNS trigger AS $$ \
             BEGIN RAISE EXCEPTION 'test write rejection' USING ERRCODE = '55000'; END; \
             $$ LANGUAGE plpgsql"
        ))
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE TRIGGER {trigger_name} BEFORE {operation} ON {table} \
             FOR EACH ROW WHEN ({when}) EXECUTE FUNCTION {function_name}()"
        ))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn reject_command_ordinal(&self, ordinal: i32) {
        self.reject_write(
            &format!("command_{ordinal}"),
            "agent_commands",
            "INSERT",
            &format!(
                "NEW.task_id = '{}'::uuid AND NEW.ordinal = {ordinal}",
                self.task_id.as_uuid()
            ),
        )
        .await;
    }

    async fn direct_runtime_transition(
        &self,
        command: &AgentCommandView,
        event_id: &str,
        outcome: &str,
        fields: Value,
    ) -> (CompleteAgentCommand, RuntimeTransition) {
        let completion = self.completion(command, event_id, outcome, fields);
        let context = self
            .store
            .load_agent_command_context(
                self.session.id.clone(),
                self.task_id.clone(),
                command.id.clone(),
            )
            .await
            .unwrap();
        let transition = self
            .runtime
            .handle(RuntimeHandle {
                execution: &context.execution,
                definition: &context.definition,
                event: &completion.event,
            })
            .unwrap();
        (completion, transition)
    }
}

#[tokio::test]
async fn command_result_replaces_snapshot_and_journals_commands_atomically() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else {
        return;
    };
    let command = fixture.claim_first("builtin-direct-v1", "claim-root").await;
    let before = fixture.current().await;
    let event = fixture.harness_result(
        &command,
        "evt-fork",
        "split",
        json!({"left":"one", "right":"two"}),
    );
    let after = fixture
        .kernel
        .complete_command(
            &fixture.principal,
            CompleteAgentCommand {
                session_id: fixture.session.id.clone(),
                task_id: fixture.task_id.clone(),
                command_id: command.id.clone(),
                expected_command_revision: command.revision,
                idempotency_key: IdempotencyKey::new("complete-root").unwrap(),
                event,
            },
        )
        .await
        .unwrap();

    assert_eq!(after.task_revision, before.task_revision + 1);
    assert_eq!(after.goal_revision, before.goal_revision);
    assert_ne!(
        after
            .runtime
            .as_ref()
            .unwrap()
            .digest(&fixture.ceilings)
            .unwrap(),
        before
            .runtime
            .as_ref()
            .unwrap()
            .digest(&fixture.ceilings)
            .unwrap()
    );
    let commands = fixture.commands_for_latest_transition().await;
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].ordinal, 0);
    assert_eq!(commands[1].ordinal, 1);
}

#[tokio::test]
async fn duplicate_event_is_one_transition_and_conflicting_content_is_rejected() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else {
        return;
    };
    let command = fixture.claim_first("builtin-direct-v1", "claim").await;
    let request = fixture.completion(&command, "evt-stable", "ok", json!({"answer":"x"}));
    let first = fixture
        .kernel
        .complete_command(&fixture.principal, request.clone())
        .await
        .unwrap();
    let replay = fixture
        .kernel
        .complete_command(&fixture.principal, request)
        .await
        .unwrap();
    assert_eq!(first.task_revision, replay.task_revision);
    assert_eq!(fixture.transition_count_for("event:evt-stable").await, 1);
    assert_eq!(fixture.runtime_handle_calls(), 1);

    let changed = fixture.completion(&command, "evt-stable", "ok", json!({"answer":"changed"}));
    assert!(matches!(
        fixture
            .kernel
            .complete_command(&fixture.principal, changed)
            .await,
        Err(StoreError::IdempotencyConflict),
    ));
    assert_eq!(fixture.transition_count_for("event:evt-stable").await, 1);

    let mut second_key = fixture.completion(&command, "evt-stable", "ok", json!({"answer":"x"}));
    second_key.idempotency_key = IdempotencyKey::new("second-completion-key").unwrap();
    let terminal = fixture.database_fingerprint().await;
    assert!(
        fixture
            .kernel
            .complete_command(&fixture.principal, second_key)
            .await
            .is_err()
    );
    assert_eq!(fixture.database_fingerprint().await, terminal);
}

#[tokio::test]
async fn concurrent_completion_races_commit_exactly_one_transition() {
    for conflict in [false, true] {
        let Some(fixture) = TransitionFixture::started_simple_agent().await else {
            return;
        };
        let command = fixture
            .claim_first("builtin-direct-v1", &format!("claim-race-{conflict}"))
            .await;
        let request = fixture.completion(
            &command,
            &format!("evt-race-{conflict}"),
            "ok",
            json!({"answer":"x"}),
        );
        let mut competing = request.clone();
        if conflict {
            match &mut competing.event {
                RuntimeEvent::HarnessResult { fields, .. } => {
                    *fields = json!({"answer":"changed"});
                }
                _ => unreachable!(),
            }
        }
        let first_kernel = Arc::clone(&fixture.kernel);
        let second_kernel = Arc::clone(&fixture.kernel);
        let first_principal = fixture.principal.clone();
        let second_principal = fixture.principal.clone();
        let (first, second) = tokio::join!(
            first_kernel.complete_command(&first_principal, request),
            second_kernel.complete_command(&second_principal, competing),
        );
        if conflict {
            assert!(
                matches!(
                    (&first, &second),
                    (Ok(_), Err(StoreError::IdempotencyConflict))
                ) || matches!(
                    (&first, &second),
                    (Err(StoreError::IdempotencyConflict), Ok(_))
                ),
                "first={first:?}, second={second:?}",
            );
        } else {
            assert_eq!(first.unwrap().task_revision, second.unwrap().task_revision);
        }
        assert_eq!(
            fixture
                .transition_count_for(&format!("event:evt-race-{conflict}"))
                .await,
            1,
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_completion_replays_when_winner_commits_after_empty_probe() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else {
        return;
    };
    let command = fixture
        .claim_first("builtin-direct-v1", "claim-stale-empty-probe")
        .await;
    let request = fixture.completion(
        &command,
        "evt-stale-empty-probe",
        "ok",
        json!({"answer":"x"}),
    );
    let reached = Arc::new(Notify::new());
    let release = Arc::new(Barrier::new(2));
    let subscriber = tracing_subscriber::registry().with(EmptyCompletionProbeGate {
        armed: Arc::new(AtomicBool::new(true)),
        reached: Arc::clone(&reached),
        release: Arc::clone(&release),
    });
    let losing_kernel = Arc::clone(&fixture.kernel);
    let losing_principal = fixture.principal.clone();
    let losing_request = request.clone();
    let loser = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(
                async move {
                    losing_kernel
                        .complete_command(&losing_principal, losing_request)
                        .await
                }
                .with_subscriber(subscriber),
            )
    });

    reached.notified().await;
    let winner = fixture
        .kernel
        .complete_command(&fixture.principal, request)
        .await
        .unwrap();
    release.wait();
    let replay = tokio::task::spawn_blocking(move || loser.join().unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(replay.task_revision, winner.task_revision);
    assert_eq!(replay.goal_revision, winner.goal_revision);
    assert_eq!(fixture.runtime.handle_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .transition_count_for("event:evt-stale-empty-probe")
            .await,
        1,
    );
}

#[tokio::test]
async fn claim_is_revisioned_idempotent_and_claimed_work_remains_dispatchable() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else {
        return;
    };
    let pending = fixture
        .store
        .dispatchable_agent_commands(64)
        .await
        .unwrap()
        .into_iter()
        .find(|command| command.task_id == fixture.task_id)
        .unwrap();
    let request = ClaimAgentCommand {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        command_id: pending.id.clone(),
        expected_command_revision: 0,
        claimant: "builtin-direct-v1".to_owned(),
        idempotency_key: IdempotencyKey::new("stable-claim").unwrap(),
    };
    let claimed = fixture
        .store
        .claim_agent_command(request.clone())
        .await
        .unwrap();
    let after_claim = fixture.database_fingerprint().await;
    let replay = fixture
        .store
        .claim_agent_command(request.clone())
        .await
        .unwrap();
    assert_eq!(claimed.state, AgentCommandState::Claimed);
    assert_eq!(claimed.revision, 1);
    assert_eq!(replay.revision, claimed.revision);
    assert_eq!(fixture.database_fingerprint().await, after_claim);
    let redelivered = fixture
        .store
        .dispatchable_agent_commands(64)
        .await
        .unwrap()
        .into_iter()
        .find(|command| command.id == claimed.id)
        .unwrap();
    assert_eq!(redelivered.claimant.as_deref(), Some("builtin-direct-v1"));
    assert_eq!(redelivered.revision, 1);

    let mut changed = request.clone();
    changed.claimant = "different-claimant".to_owned();
    assert!(matches!(
        fixture.store.claim_agent_command(changed).await,
        Err(StoreError::IdempotencyConflict)
    ));
    let mut second_key = request;
    second_key.idempotency_key = IdempotencyKey::new("second-claim-key").unwrap();
    second_key.expected_command_revision = 1;
    assert!(matches!(
        fixture.store.claim_agent_command(second_key).await,
        Err(StoreError::RevisionConflict)
    ));
}

#[tokio::test]
async fn pending_command_cannot_complete_or_fail_and_writes_nothing() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else {
        return;
    };
    let pending = fixture
        .store
        .dispatchable_agent_commands(64)
        .await
        .unwrap()
        .into_iter()
        .find(|command| command.task_id == fixture.task_id)
        .unwrap();
    assert_eq!(pending.state, AgentCommandState::Pending);
    let before = fixture.database_fingerprint().await;
    let handle_calls = fixture.runtime_handle_calls();
    let completion = fixture.completion(
        &pending,
        "evt-pending-command",
        "ok",
        json!({"answer":"must-not-commit"}),
    );

    assert!(matches!(
        fixture
            .kernel
            .complete_command(&fixture.principal, completion.clone())
            .await,
        Err(StoreError::RevisionConflict)
    ));
    assert_eq!(fixture.runtime_handle_calls(), handle_calls);

    let context = fixture
        .store
        .load_agent_command_context(
            fixture.session.id.clone(),
            fixture.task_id.clone(),
            pending.id.clone(),
        )
        .await
        .unwrap();
    let transition = fixture
        .runtime
        .handle(RuntimeHandle {
            execution: &context.execution,
            definition: &context.definition,
            event: &completion.event,
        })
        .unwrap();
    assert!(matches!(
        fixture
            .store
            .commit_agent_command_transition(
                &fixture.principal,
                &completion,
                &transition,
                None,
                &fixture.ceilings,
            )
            .await,
        Err(StoreError::RevisionConflict)
    ));

    let unavailable = FailAgentCommand {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        command_id: pending.id,
        expected_command_revision: pending.revision,
        idempotency_key: IdempotencyKey::new("unavailable-pending-command").unwrap(),
        code: "runtime_command_unavailable".to_owned(),
    };
    assert!(matches!(
        fixture
            .store
            .fail_unavailable_agent_command(&fixture.principal, &unavailable)
            .await,
        Err(StoreError::RevisionConflict)
    ));
    assert_eq!(fixture.database_fingerprint().await, before);
}

#[tokio::test]
async fn structured_partial_join_keeps_the_persisted_sibling_without_reproposal() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else {
        return;
    };
    let root = fixture.claim_first("builtin-direct-v1", "claim-root").await;
    fixture
        .kernel
        .complete_command(
            &fixture.principal,
            fixture.completion(
                &root,
                "evt-root-split",
                "split",
                json!({"left":"one", "right":"two"}),
            ),
        )
        .await
        .unwrap();
    let children = fixture.commands_for_latest_transition().await;
    assert_eq!(children.len(), 2);
    let left = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: children[0].id.clone(),
            expected_command_revision: 0,
            claimant: "builtin-direct-v1".to_owned(),
            idempotency_key: IdempotencyKey::new("claim-left").unwrap(),
        })
        .await
        .unwrap();
    let after_left = fixture
        .kernel
        .complete_command(
            &fixture.principal,
            fixture.completion(&left, "evt-left", "ok", json!({"result":"left-result"})),
        )
        .await
        .unwrap();
    assert_eq!(after_left.lifecycle, TaskLifecycle::Running);
    assert_eq!(
        after_left.operational_status,
        OperationalStatus::WaitingForHarness
    );
    let rows = fixture.command_rows().await;
    assert_eq!(rows.iter().filter(|row| row.1 == "pending").count(), 1);
    assert!(rows.iter().any(|row| row.0 == children[1].key.as_str()));
    assert_eq!(fixture.commands_for_latest_transition().await.len(), 0);
}

#[tokio::test]
async fn running_partial_join_cannot_persist_fail_task_and_writes_nothing() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else {
        return;
    };
    let root = fixture
        .claim_first("builtin-direct-v1", "claim-fail-shape-root")
        .await;
    fixture
        .kernel
        .complete_command(
            &fixture.principal,
            fixture.completion(
                &root,
                "evt-fail-shape-root",
                "split",
                json!({"left":"one", "right":"two"}),
            ),
        )
        .await
        .unwrap();
    let children = fixture.commands_for_latest_transition().await;
    let source = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: children[0].id.clone(),
            expected_command_revision: children[0].revision,
            claimant: "builtin-direct-v1".to_owned(),
            idempotency_key: IdempotencyKey::new("claim-fail-shape-child").unwrap(),
        })
        .await
        .unwrap();
    let (completion, mut transition) = fixture
        .direct_runtime_transition(
            &source,
            "evt-fail-shape-child",
            "ok",
            json!({"result":"left-result"}),
        )
        .await;
    assert_eq!(transition.snapshot.disposition, RuntimeDisposition::Running);
    assert!(transition.commands.is_empty());
    transition.commands.push(KernelCommand {
        schema_version: 1,
        key: RuntimeCommandKey::new(format!(
            "r{}-task-fail",
            transition.expected_task_revision + 1
        ))
        .unwrap(),
        payload: KernelCommandPayload::FailTask(FailTask {
            code: "forged_failure".to_owned(),
            message: "must not persist".to_owned(),
        }),
    });
    let before = fixture.database_fingerprint().await;

    assert!(matches!(
        fixture
            .store
            .commit_agent_command_transition(
                &fixture.principal,
                &completion,
                &transition,
                None,
                &fixture.ceilings,
            )
            .await,
        Err(StoreError::AgentTransitionInvalid(_))
    ));
    assert_eq!(fixture.database_fingerprint().await, before);
}

#[tokio::test]
async fn matching_wait_and_command_with_invalid_revision_are_rejected_without_writes() {
    for revision in ["0", "999", "02"] {
        let Some(fixture) = TransitionFixture::started_fork_agent().await else {
            return;
        };
        let source = fixture
            .claim_first(
                "builtin-direct-v1",
                &format!("claim-invalid-wait-{revision}"),
            )
            .await;
        let (completion, mut transition) = fixture
            .direct_runtime_transition(
                &source,
                &format!("evt-invalid-wait-{revision}"),
                "split",
                json!({"left":"one", "right":"two"}),
            )
            .await;
        let original_key = transition.commands[0].key.clone();
        let mut state = fixture
            .runtime
            .inner
            .decode_snapshot(&transition.snapshot)
            .unwrap();
        let frame = state
            .frames
            .values_mut()
            .find(|frame| {
                matches!(
                    &frame.state,
                    KiteFrameState::WaitingForHarness { command_key }
                        if command_key == &original_key
                )
            })
            .unwrap();
        let invalid_key =
            RuntimeCommandKey::new(format!("r{revision}-f{}-harness", frame.id.get())).unwrap();
        frame.state = KiteFrameState::WaitingForHarness {
            command_key: invalid_key.clone(),
        };
        transition.commands[0].key = invalid_key;
        transition.snapshot = RuntimeStateSnapshot::new(
            transition.snapshot.disposition,
            serde_json::to_value(state).unwrap(),
            &fixture.ceilings,
        )
        .unwrap();
        let before = fixture.database_fingerprint().await;

        assert!(matches!(
            fixture
                .store
                .commit_agent_command_transition(
                    &fixture.principal,
                    &completion,
                    &transition,
                    None,
                    &fixture.ceilings,
                )
                .await,
            Err(StoreError::AgentTransitionInvalid(_))
        ));
        assert_eq!(fixture.database_fingerprint().await, before, "{revision}");
    }
}

#[tokio::test]
async fn forged_idle_running_snapshot_is_rejected_without_writes() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else {
        return;
    };
    let command = fixture.claim_first("builtin-direct-v1", "claim-idle").await;
    let current = fixture.current().await;
    let mut state: KiteRuntimeState =
        serde_json::from_value(current.runtime.as_ref().unwrap().payload.clone()).unwrap();
    state.frames.get_mut(&KiteFrameId::new(0)).unwrap().state = KiteFrameState::Ready;
    let completion = fixture.completion(
        &command,
        "evt-forged-idle",
        "ok",
        json!({"answer":"ignored"}),
    );
    let transition = RuntimeTransition {
        idempotency_key: "event:evt-forged-idle".to_owned(),
        expected_task_revision: current.task_revision,
        expected_goal_revision: current.goal_revision,
        snapshot: RuntimeStateSnapshot::new(
            RuntimeDisposition::Running,
            serde_json::to_value(state).unwrap(),
            &fixture.ceilings,
        )
        .unwrap(),
        commands: vec![],
    };
    let before = fixture.database_fingerprint().await;
    assert!(matches!(
        fixture
            .store
            .commit_agent_command_transition(
                &fixture.principal,
                &completion,
                &transition,
                None,
                &fixture.ceilings,
            )
            .await,
        Err(StoreError::AgentTransitionInvalid(message))
            if message.contains("structured partial join")
    ));
    assert_eq!(fixture.database_fingerprint().await, before);
}

#[tokio::test]
async fn event_kind_must_match_the_persisted_command_before_any_write() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else {
        return;
    };
    let command = fixture
        .claim_first("builtin-direct-v1", "claim-event-kind")
        .await;
    let (mut completion, transition) = fixture
        .direct_runtime_transition(&command, "evt-event-kind", "ok", json!({"answer":"x"}))
        .await;
    completion.event = RuntimeEvent::ContextResolved {
        event_id: "evt-event-kind".to_owned(),
        command_key: command.key,
        evidence_ids: vec![],
    };
    let before = fixture.database_fingerprint().await;
    assert!(matches!(
        fixture
            .store
            .commit_agent_command_transition(
                &fixture.principal,
                &completion,
                &transition,
                None,
                &fixture.ceilings,
            )
            .await,
        Err(StoreError::AgentTransitionInvalid(message)) if message.contains("event kind")
    ));
    assert_eq!(fixture.database_fingerprint().await, before);
}

#[tokio::test]
async fn stale_foreign_and_malformed_proposals_are_rejected_without_writes() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else {
        return;
    };
    let command = fixture
        .claim_first("builtin-direct-v1", "claim-negative-cas")
        .await;
    let (base_completion, base_transition) = fixture
        .direct_runtime_transition(
            &command,
            "evt-negative-cas",
            "split",
            json!({"left":"one", "right":"two"}),
        )
        .await;
    let before = fixture.database_fingerprint().await;

    for case in [
        "stale_task",
        "stale_goal",
        "stale_command",
        "foreign_task",
        "foreign_session",
        "foreign_principal",
        "command_key",
        "source_event_id",
        "snapshot_digest",
        "unknown_command_schema",
        "duplicate_command_key",
        "oversized_snapshot",
        "oversized_command",
        "oversized_history",
        "mixed_external_kinds",
        "complete_goal_authority",
        "block_goal_authority",
    ] {
        let mut completion = base_completion.clone();
        let mut transition = base_transition.clone();
        let mut principal = fixture.principal.clone();
        match case {
            "stale_task" => transition.expected_task_revision -= 1,
            "stale_goal" => transition.expected_goal_revision += 1,
            "stale_command" => completion.expected_command_revision += 1,
            "foreign_task" => completion.task_id = UserTaskId::new(Uuid::now_v7()),
            "foreign_session" => completion.session_id = SessionId::new(Uuid::now_v7()),
            "foreign_principal" => {
                principal = PrincipalId::new(format!("foreign-{}", Uuid::now_v7())).unwrap();
            }
            "command_key" => match &mut completion.event {
                RuntimeEvent::HarnessResult { command_key, .. } => {
                    *command_key = RuntimeCommandKey::new("forged-command-key").unwrap();
                }
                _ => unreachable!(),
            },
            "source_event_id" => match &mut completion.event {
                RuntimeEvent::HarnessResult { event_id, .. } => {
                    *event_id = "evt-forged-source".to_owned();
                }
                _ => unreachable!(),
            },
            "snapshot_digest" => {
                transition.snapshot.snapshot_digest = "sha256:corrupt".to_owned();
            }
            "unknown_command_schema" => transition.commands[0].schema_version = 2,
            "duplicate_command_key" => {
                transition.commands[1].key = transition.commands[0].key.clone();
            }
            "oversized_snapshot" => {
                transition.snapshot.payload =
                    json!({"oversized": "x".repeat(fixture.ceilings.max_snapshot_bytes)});
            }
            "oversized_command" => match &mut transition.commands[0].payload {
                KernelCommandPayload::InvokeHarness(command) => {
                    command.prompt = "x".repeat(fixture.ceilings.max_command_bytes + 1);
                }
                _ => unreachable!(),
            },
            "oversized_history" => {
                let mut state = fixture
                    .runtime
                    .inner
                    .decode_snapshot(&transition.snapshot)
                    .unwrap();
                state.frames.values_mut().next().unwrap().history = vec![
                    RuntimeHistoryItem {
                        sequence: 0,
                        kind: RuntimeHistoryKind::HarnessInput,
                        payload: Value::Null,
                    };
                    fixture.ceilings.max_history_items_per_frame
                        + 1
                ];
                transition.snapshot = RuntimeStateSnapshot::new(
                    transition.snapshot.disposition,
                    serde_json::to_value(state).unwrap(),
                    &fixture.ceilings,
                )
                .unwrap();
            }
            "mixed_external_kinds" => {
                transition.commands[1].payload =
                    KernelCommandPayload::RequestApproval(RequestApproval {
                        question: "Approve?".to_owned(),
                        answer_schema: ClosedSchema::object("answer", Default::default()),
                    });
            }
            "complete_goal_authority" => {
                transition.commands[0].payload =
                    KernelCommandPayload::CompleteGoal(CompleteGoal { assertions: vec![] });
            }
            "block_goal_authority" => {
                transition.commands[0].payload = KernelCommandPayload::BlockGoal(BlockGoal {
                    blocked_when_index: 0,
                    reason_code: "blocked".to_owned(),
                    explanation: "not yet".to_owned(),
                    evidence_ids: vec![],
                    unlock_conditions: vec!["retry".to_owned()],
                });
            }
            _ => unreachable!(),
        }
        assert!(
            fixture
                .store
                .commit_agent_command_transition(
                    &principal,
                    &completion,
                    &transition,
                    None,
                    &fixture.ceilings,
                )
                .await
                .is_err(),
            "{case}",
        );
        assert_eq!(fixture.database_fingerprint().await, before, "{case}");
    }
}

#[tokio::test]
async fn a_surviving_wait_requires_a_pending_or_claimed_command_of_the_same_kind() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else {
        return;
    };
    let root = fixture.claim_first("builtin-direct-v1", "claim-root").await;
    fixture
        .kernel
        .complete_command(
            &fixture.principal,
            fixture.completion(
                &root,
                "evt-root-for-sibling",
                "split",
                json!({"left":"one", "right":"two"}),
            ),
        )
        .await
        .unwrap();
    let children = fixture.commands_for_latest_transition().await;
    let left = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: children[0].id.clone(),
            expected_command_revision: 0,
            claimant: "builtin-direct-v1".to_owned(),
            idempotency_key: IdempotencyKey::new("claim-left-mismatch").unwrap(),
        })
        .await
        .unwrap();
    sqlx::query(
        "UPDATE agent_commands SET state = 'completed', revision = revision + 1 \
         WHERE task_id = $1 AND id = $2",
    )
    .bind(fixture.task_id.as_uuid())
    .bind(children[1].id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let before = fixture.database_fingerprint().await;
    assert!(
        fixture
            .kernel
            .complete_command(
                &fixture.principal,
                fixture.completion(
                    &left,
                    "evt-left-missing-sibling",
                    "ok",
                    json!({"result":"left-result"}),
                ),
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.database_fingerprint().await, before);
    sqlx::query(
        "UPDATE agent_commands SET state = 'pending', revision = revision - 1 \
         WHERE task_id = $1 AND id = $2",
    )
    .bind(fixture.task_id.as_uuid())
    .bind(children[1].id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn second_command_insert_failure_rolls_back_source_completion() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else {
        return;
    };
    let command = fixture.claim_first("builtin-direct-v1", "claim-root").await;
    fixture.reject_command_ordinal(1).await;
    let before = fixture.database_fingerprint().await;
    assert!(
        fixture
            .kernel
            .complete_command(
                &fixture.principal,
                fixture.completion(
                    &command,
                    "evt-fork-rollback",
                    "split",
                    json!({"left":"one", "right":"two"}),
                ),
            )
            .await
            .is_err()
    );
    assert_eq!(fixture.database_fingerprint().await, before);
}

#[tokio::test]
async fn snapshot_transition_and_event_failures_each_roll_back_the_whole_cas() {
    for (name, table, operation, row) in [
        ("snapshot", "agent_runtime_snapshots", "UPDATE", "OLD"),
        ("transition", "agent_transitions", "INSERT", "NEW"),
        ("event", "agent_task_events", "INSERT", "NEW"),
    ] {
        let Some(fixture) = TransitionFixture::started_simple_agent().await else {
            return;
        };
        let command = fixture
            .claim_first("builtin-direct-v1", &format!("claim-{name}"))
            .await;
        fixture
            .reject_write(
                name,
                table,
                operation,
                &format!("{row}.task_id = '{}'::uuid", fixture.task_id.as_uuid()),
            )
            .await;
        let before = fixture.database_fingerprint().await;
        assert!(
            fixture
                .kernel
                .complete_command(
                    &fixture.principal,
                    fixture.completion(
                        &command,
                        &format!("evt-{name}-rollback"),
                        "ok",
                        json!({"answer":"x"}),
                    ),
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.database_fingerprint().await, before, "{name}");
    }
}

#[tokio::test]
async fn definitive_command_failure_is_terminal_idempotent_and_goal_neutral() {
    let Some(fixture) = TransitionFixture::started_simple_agent().await else {
        return;
    };
    let command = fixture.claim_first("builtin-direct-v1", "claim-fail").await;
    let request = FailAgentCommand {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        command_id: command.id.clone(),
        expected_command_revision: command.revision,
        idempotency_key: IdempotencyKey::new("fail-command").unwrap(),
        code: "harness_failed".to_owned(),
    };
    let failed = fixture
        .kernel
        .fail_command(&fixture.principal, request.clone())
        .await
        .unwrap();
    let replay = fixture
        .kernel
        .fail_command(&fixture.principal, request.clone())
        .await
        .unwrap();
    assert_eq!(failed.lifecycle, TaskLifecycle::Failed);
    assert_eq!(failed.operational_status, OperationalStatus::Finished);
    assert_eq!(failed.goal_phase, GoalPhase::Active);
    assert_eq!(failed.goal_revision, 0);
    assert_eq!(replay.task_revision, failed.task_revision);
    assert_eq!(fixture.runtime_handle_calls(), 1);
    let rows = fixture.command_rows().await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.1 == "completed"));

    let mut changed = request;
    changed.code = "different_failure".to_owned();
    assert!(matches!(
        fixture
            .kernel
            .fail_command(&fixture.principal, changed)
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(fixture.runtime_handle_calls(), 1);
}

#[tokio::test]
async fn unavailable_command_fails_atomically_without_invoking_the_runtime() {
    let Some(fixture) = TransitionFixture::started_fork_agent().await else {
        return;
    };
    let root = fixture
        .claim_first("builtin-direct-v1", "claim-unavailable-root")
        .await;
    fixture
        .kernel
        .complete_command(
            &fixture.principal,
            fixture.completion(
                &root,
                "evt-unavailable-root",
                "split",
                json!({"left":"one", "right":"two"}),
            ),
        )
        .await
        .unwrap();
    let source = fixture
        .claim_first("builtin-direct-v1", "claim-unavailable-child")
        .await;
    let before = fixture.current().await;
    let before_snapshot: (Value, String, i32, i32) = sqlx::query_as(
        "SELECT payload, snapshot_digest, canonical_bytes, ceiling_bytes \
         FROM agent_runtime_snapshots WHERE task_id = $1",
    )
    .bind(fixture.task_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let handle_calls = fixture.runtime_handle_calls();
    let request = FailAgentCommand {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        command_id: source.id.clone(),
        expected_command_revision: source.revision,
        idempotency_key: IdempotencyKey::new("unavailable-child").unwrap(),
        code: "runtime_command_unavailable".to_owned(),
    };

    let failed = fixture
        .store
        .fail_unavailable_agent_command(&fixture.principal, &request)
        .await
        .unwrap();
    let replay = fixture
        .store
        .fail_unavailable_agent_command(&fixture.principal, &request)
        .await
        .unwrap();

    assert_eq!(failed.lifecycle, TaskLifecycle::Failed);
    assert_eq!(failed.operational_status, OperationalStatus::Finished);
    assert_eq!(failed.task_revision, before.task_revision + 1);
    assert_eq!(failed.goal_revision, before.goal_revision);
    assert_eq!(replay.task_revision, failed.task_revision);
    assert_eq!(fixture.runtime_handle_calls(), handle_calls);
    let foreign = PrincipalId::new(format!("foreign-replay-{}", Uuid::now_v7())).unwrap();
    assert!(matches!(
        fixture
            .store
            .fail_unavailable_agent_command(&foreign, &request)
            .await,
        Err(StoreError::PrincipalMismatch)
    ));
    let after_snapshot: (Value, String, i32, i32) = sqlx::query_as(
        "SELECT payload, snapshot_digest, canonical_bytes, ceiling_bytes \
         FROM agent_runtime_snapshots WHERE task_id = $1",
    )
    .bind(fixture.task_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(after_snapshot, before_snapshot);
    let rows = fixture.command_rows().await;
    assert!(
        rows.iter()
            .any(|row| row.0 == source.key.as_str() && row.1 == "failed")
    );
    assert!(
        rows.iter()
            .any(|row| row.0 != source.key.as_str() && row.1 == "cancelled")
    );

    let mut changed = request.clone();
    changed.expected_command_revision += 1;
    assert!(matches!(
        fixture
            .store
            .fail_unavailable_agent_command(&fixture.principal, &changed)
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    let mut invalid = request;
    invalid.idempotency_key = IdempotencyKey::new("unavailable-invalid-code").unwrap();
    invalid.code = "other".to_owned();
    assert!(matches!(
        fixture
            .store
            .fail_unavailable_agent_command(&fixture.principal, &invalid)
            .await,
        Err(StoreError::AgentTransitionInvalid(_))
    ));
}
