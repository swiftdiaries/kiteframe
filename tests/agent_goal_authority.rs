use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    AgentCommandView, AgentExecutionView, AgentKernel, AgentLifecycleAction,
    AgentLifecycleAuthorizer, AgentPolicyDecision, AgentRuntime, AgentRuntimeError, AgentSelector,
    CancelUserTask, ClaimAgentCommand, CompleteAgentCommand, CreateSession, CriterionAssertion,
    EvidenceId, FailAgentCommand, GoalContract, GoalPhase, IdempotencyKey, KernelCommandPayload,
    KiteAgentRuntime, OperationalStatus, PauseUserTask, PolicyRevision, PostgresStore, PrincipalId,
    ResumeUserTask, RuntimeCeilings, RuntimeEvent, RuntimeHandle, RuntimeStart, RuntimeTransition,
    SessionView, StartAgent, StoreError, SubmitUserTask, SuccessCriterion, TaskLifecycle,
    TaskOwnerAuthorizer, TaskPreferences, TaskTarget, ToolCatalogSnapshot, UserTaskId,
    goal_edge_allowed,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct GoalFixture {
    kernel: Arc<AgentKernel>,
    store: Arc<PostgresStore>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    task_id: UserTaskId,
}

struct PreparedGoalCompletion {
    completion: CompleteAgentCommand,
    transition: RuntimeTransition,
    policy: AgentPolicyDecision,
}

struct RecordingRuntime {
    inner: KiteAgentRuntime,
    handle_calls: AtomicUsize,
}

impl RecordingRuntime {
    fn new() -> Self {
        Self {
            inner: KiteAgentRuntime::new(RuntimeCeilings::default()),
            handle_calls: AtomicUsize::new(0),
        }
    }

    fn handle_calls(&self) -> usize {
        self.handle_calls.load(Ordering::SeqCst)
    }
}

impl AgentRuntime for RecordingRuntime {
    fn validate_agent(
        &self,
        definition: &kiteframe::dsl::CompiledDefinition,
    ) -> Result<(), AgentRuntimeError> {
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

impl GoalFixture {
    async fn three_harness_criteria() -> Option<Self> {
        Self::started(
            r#"task inspect(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Inspect.""" tools []
    on proceed() {
      first = call collect() context isolated
      second = call collect() context isolated
      complete goal {
        reviews_resolved: current_evidence
        checks_passed: first
        pull_request_merged: second
      }
    }
  }
  task collect() -> string {
    model reasoning prompt """Collect.""" tools []
    on observed() { done current_evidence }
  }"#,
            vec![
                ("reviews_resolved", "Reviews are resolved"),
                ("checks_passed", "Checks pass"),
                ("pull_request_merged", "The pull request is merged"),
            ],
            vec![],
        )
        .await
    }

    async fn blocked_conditions() -> Option<Self> {
        Self::started(
            r#"task inspect(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Inspect.""" tools []
    on no_path() {
      block goal when 1 {
        code: "no_policy_permitted_path"
        explanation: "All currently permitted repair paths were exhausted."
        evidence: [current_evidence]
        unlock: ["A policy revision permits a bounded repair."]
      }
    }
  }"#,
            vec![("finished", "The task is finished")],
            vec![
                "Required user action remains unavailable",
                "No policy-permitted path remains",
            ],
        )
        .await
    }

    async fn complete_or_block() -> Option<Self> {
        Self::started(
            r#"task inspect(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Inspect.""" tools []
    on finished() {
      complete goal { finished: current_evidence }
    }
    on no_path() {
      block goal when 0 {
        code: "no_policy_permitted_path"
        explanation: "No permitted path remains."
        evidence: [current_evidence]
        unlock: ["A policy revision permits repair."]
      }
    }
  }"#,
            vec![("finished", "The task is finished")],
            vec!["No policy-permitted path remains"],
        )
        .await
    }

    async fn blocked_with_prior_evidence() -> Option<Self> {
        Self::started(
            r#"task inspect(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Inspect.""" tools []
    on no_path() {
      proof = call collect() context isolated
      block goal when 1 {
        code: "no_policy_permitted_path"
        explanation: "All currently permitted repair paths were exhausted."
        evidence: [current_evidence]
        unlock: ["A policy revision permits a bounded repair."]
      }
    }
  }
  task collect() -> string {
    model reasoning prompt """Collect.""" tools []
    on observed() { done current_evidence }
  }"#,
            vec![("finished", "The task is finished")],
            vec![
                "Required user action remains unavailable",
                "No policy-permitted path remains",
            ],
        )
        .await
    }

    async fn started(
        tasks: &str,
        criteria: Vec<(&str, &str)>,
        blocked_when: Vec<&str>,
    ) -> Option<Self> {
        let fixture = Self::accepted(tasks, criteria, blocked_when).await?;
        fixture
            .kernel
            .start_agent(
                &fixture.principal,
                StartAgent {
                    session_id: fixture.session.id.clone(),
                    task_id: fixture.task_id.clone(),
                    expected_task_revision: 0,
                    expected_goal_revision: 0,
                    idempotency_key: IdempotencyKey::new(format!("start-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        Some(fixture)
    }

    async fn accepted(
        tasks: &str,
        criteria: Vec<(&str, &str)>,
        blocked_when: Vec<&str>,
    ) -> Option<Self> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let principal = PrincipalId::new(format!("goal-authority-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/goal-authority".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
        store.append_catalog_epoch(&catalog).await.unwrap();
        let name = format!("goal_authority_{}", Uuid::now_v7().simple());
        let source = format!(
            r#"agent {name}(repository: string, pull_request: integer) -> string {{
  entry inspect(repository = repository, pull_request = pull_request)
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
                        objective: "Finish the accepted task".to_owned(),
                        success: criteria
                            .into_iter()
                            .map(|(id, statement)| SuccessCriterion {
                                id: id.to_owned(),
                                statement: statement.to_owned(),
                                evidence: "harness.invocation_result".to_owned(),
                            })
                            .collect(),
                        constraints: vec![],
                        blocked_when: blocked_when.into_iter().map(str::to_owned).collect(),
                    },
                    idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let kernel = Arc::new(AgentKernel::new(
            Arc::clone(&store),
            Arc::new(KiteAgentRuntime::new(RuntimeCeilings::default())),
            Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&store))),
        ));
        Some(Self {
            kernel,
            pool: store.pool().clone(),
            store,
            session,
            principal,
            task_id: accepted.id,
        })
    }

    async fn current(&self) -> AgentExecutionView {
        self.store
            .get_agent_execution(self.session.id.clone(), self.task_id.clone())
            .await
            .unwrap()
    }

    async fn claim_next(&self) -> AgentCommandView {
        let command = self
            .store
            .dispatchable_agent_commands(256)
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
                claimant: "builtin-direct-v1".to_owned(),
                idempotency_key: IdempotencyKey::new(format!("claim-{}", Uuid::now_v7())).unwrap(),
            })
            .await
            .unwrap()
    }

    async fn complete(
        &self,
        command: &AgentCommandView,
        outcome: &str,
        fields: Value,
        evidence_id: EvidenceId,
    ) -> Result<AgentExecutionView, StoreError> {
        let event_id = format!("event-{}", Uuid::now_v7());
        self.kernel
            .complete_command(
                &self.principal,
                CompleteAgentCommand {
                    session_id: self.session.id.clone(),
                    task_id: self.task_id.clone(),
                    command_id: command.id.clone(),
                    expected_command_revision: command.revision,
                    idempotency_key: IdempotencyKey::new(format!("complete-{}", Uuid::now_v7()))
                        .unwrap(),
                    event: RuntimeEvent::HarnessResult {
                        event_id,
                        command_key: command.key.clone(),
                        outcome: outcome.to_owned(),
                        fields,
                        evidence_id,
                        history: vec![],
                    },
                },
            )
            .await
    }

    async fn complete_three_criterion_goal(&self) -> Result<AgentExecutionView, StoreError> {
        let root = self.claim_next().await;
        self.complete(&root, "proceed", json!({}), EvidenceId::new(Uuid::now_v7()))
            .await?;
        let first = self.claim_next().await;
        self.complete(
            &first,
            "observed",
            json!({}),
            EvidenceId::new(Uuid::now_v7()),
        )
        .await?;
        let second = self.claim_next().await;
        self.complete(
            &second,
            "observed",
            json!({}),
            EvidenceId::new(Uuid::now_v7()),
        )
        .await
    }

    async fn prepare_three_criterion_completion(&self) -> PreparedGoalCompletion {
        let root = self.claim_next().await;
        self.complete(&root, "proceed", json!({}), EvidenceId::new(Uuid::now_v7()))
            .await
            .unwrap();
        let first = self.claim_next().await;
        self.complete(
            &first,
            "observed",
            json!({}),
            EvidenceId::new(Uuid::now_v7()),
        )
        .await
        .unwrap();
        let source = self.claim_next().await;
        let event_id = format!("event-{}", Uuid::now_v7());
        let completion = CompleteAgentCommand {
            session_id: self.session.id.clone(),
            task_id: self.task_id.clone(),
            command_id: source.id.clone(),
            expected_command_revision: source.revision,
            idempotency_key: IdempotencyKey::new(format!("complete-{}", Uuid::now_v7())).unwrap(),
            event: RuntimeEvent::HarnessResult {
                event_id,
                command_key: source.key,
                outcome: "observed".to_owned(),
                fields: json!({}),
                evidence_id: EvidenceId::new(Uuid::now_v7()),
                history: vec![],
            },
        };
        let context = self
            .store
            .load_agent_command_context(self.session.id.clone(), self.task_id.clone(), source.id)
            .await
            .unwrap();
        let transition = KiteAgentRuntime::new(RuntimeCeilings::default())
            .handle(RuntimeHandle {
                execution: &context.execution,
                definition: &context.definition,
                event: &completion.event,
            })
            .unwrap();
        let policy = TaskOwnerAuthorizer::new(Arc::clone(&self.store))
            .authorize(
                &self.principal,
                &context.execution.task,
                AgentLifecycleAction::CompleteGoal,
            )
            .await
            .unwrap();
        PreparedGoalCompletion {
            completion,
            transition,
            policy,
        }
    }

    async fn prepare_block_completion(&self) -> PreparedGoalCompletion {
        let root = self.claim_next().await;
        self.complete(&root, "no_path", json!({}), EvidenceId::new(Uuid::now_v7()))
            .await
            .unwrap();
        let source = self.claim_next().await;
        let event_id = format!("event-{}", Uuid::now_v7());
        let completion = CompleteAgentCommand {
            session_id: self.session.id.clone(),
            task_id: self.task_id.clone(),
            command_id: source.id.clone(),
            expected_command_revision: source.revision,
            idempotency_key: IdempotencyKey::new(format!("complete-{}", Uuid::now_v7())).unwrap(),
            event: RuntimeEvent::HarnessResult {
                event_id,
                command_key: source.key,
                outcome: "observed".to_owned(),
                fields: json!({}),
                evidence_id: EvidenceId::new(Uuid::now_v7()),
                history: vec![],
            },
        };
        let context = self
            .store
            .load_agent_command_context(self.session.id.clone(), self.task_id.clone(), source.id)
            .await
            .unwrap();
        let transition = KiteAgentRuntime::new(RuntimeCeilings::default())
            .handle(RuntimeHandle {
                execution: &context.execution,
                definition: &context.definition,
                event: &completion.event,
            })
            .unwrap();
        let policy = TaskOwnerAuthorizer::new(Arc::clone(&self.store))
            .authorize(
                &self.principal,
                &context.execution.task,
                AgentLifecycleAction::BlockGoal,
            )
            .await
            .unwrap();
        PreparedGoalCompletion {
            completion,
            transition,
            policy,
        }
    }

    async fn prepare_claimed_authority(
        &self,
        source: &AgentCommandView,
        outcome: &str,
    ) -> PreparedGoalCompletion {
        let completion = CompleteAgentCommand {
            session_id: self.session.id.clone(),
            task_id: self.task_id.clone(),
            command_id: source.id.clone(),
            expected_command_revision: source.revision,
            idempotency_key: IdempotencyKey::new(format!("complete-{}", Uuid::now_v7())).unwrap(),
            event: RuntimeEvent::HarnessResult {
                event_id: format!("event-{}", Uuid::now_v7()),
                command_key: source.key.clone(),
                outcome: outcome.to_owned(),
                fields: json!({}),
                evidence_id: EvidenceId::new(Uuid::now_v7()),
                history: vec![],
            },
        };
        let context = self
            .store
            .load_agent_command_context(
                self.session.id.clone(),
                self.task_id.clone(),
                source.id.clone(),
            )
            .await
            .unwrap();
        let transition = KiteAgentRuntime::new(RuntimeCeilings::default())
            .handle(RuntimeHandle {
                execution: &context.execution,
                definition: &context.definition,
                event: &completion.event,
            })
            .unwrap();
        let action = match transition.commands.as_slice() {
            [command] if matches!(command.payload, KernelCommandPayload::CompleteGoal(_)) => {
                AgentLifecycleAction::CompleteGoal
            }
            [command] if matches!(command.payload, KernelCommandPayload::BlockGoal(_)) => {
                AgentLifecycleAction::BlockGoal
            }
            _ => panic!("outcome must propose one goal authority command"),
        };
        let policy = TaskOwnerAuthorizer::new(Arc::clone(&self.store))
            .authorize(&self.principal, &context.execution.task, action)
            .await
            .unwrap();
        PreparedGoalCompletion {
            completion,
            transition,
            policy,
        }
    }

    async fn commit_prepared(
        &self,
        prepared: &PreparedGoalCompletion,
    ) -> Result<AgentExecutionView, StoreError> {
        self.store
            .commit_agent_command_transition(
                &self.principal,
                &prepared.completion,
                &prepared.transition,
                Some(&prepared.policy),
                &RuntimeCeilings::default(),
            )
            .await
    }

    async fn database_fingerprint(&self) -> Value {
        let authority: Value = sqlx::query_scalar(
            "SELECT jsonb_build_object( \
                 'task', (SELECT jsonb_build_array(state, operational_status, revision) \
                          FROM user_tasks WHERE id = $1), \
                 'goal', (SELECT jsonb_build_array(phase, revision, current_blocker_id) \
                          FROM goal_states WHERE task_id = $1), \
                 'snapshot', (SELECT jsonb_build_array(task_revision, snapshot_digest) \
                              FROM agent_runtime_snapshots WHERE task_id = $1), \
                 'commands', (SELECT COALESCE(jsonb_agg(jsonb_build_array( \
                     id, state, revision, result, failure) ORDER BY id), '[]'::jsonb) \
                     FROM agent_commands WHERE task_id = $1), \
                 'evidence', (SELECT COUNT(*) FROM goal_evidence WHERE task_id = $1), \
                 'assertions', (SELECT COUNT(*) FROM goal_criterion_assertions WHERE task_id = $1), \
                 'blockers', (SELECT COUNT(*) FROM goal_blockers WHERE task_id = $1), \
                 'transitions', (SELECT COUNT(*) FROM agent_transitions WHERE task_id = $1), \
                 'policies', (SELECT COUNT(*) FROM agent_policy_decisions WHERE task_id = $1), \
                 'actions', (SELECT COUNT(*) FROM agent_task_actions WHERE task_id = $1), \
                 'events', (SELECT COUNT(*) FROM agent_task_events WHERE task_id = $1))",
        )
        .bind(self.task_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .unwrap();
        authority
    }

    async fn assertions(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM goal_criterion_assertions WHERE task_id = $1")
            .bind(self.task_id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn blocker_history(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM goal_blockers WHERE task_id = $1")
            .bind(self.task_id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

#[derive(Clone, Copy)]
enum ExpectedStoreError {
    Idempotency,
    Incomplete,
    NotFound,
    TypeMismatch,
    Unauthorized,
    Stale,
    TaskRevision,
    GoalRevision,
    PolicyRevision,
    TransitionInvalid,
    Terminal,
}

impl ExpectedStoreError {
    fn matches(self, error: &StoreError) -> bool {
        match self {
            Self::Idempotency => matches!(error, StoreError::IdempotencyConflict),
            Self::Incomplete => matches!(error, StoreError::GoalEvidenceIncomplete),
            Self::NotFound => matches!(error, StoreError::GoalEvidenceNotFound),
            Self::TypeMismatch => matches!(error, StoreError::GoalEvidenceTypeMismatch),
            Self::Unauthorized => matches!(error, StoreError::GoalEvidenceUnauthorized),
            Self::Stale => matches!(error, StoreError::GoalEvidenceStale),
            Self::TaskRevision => matches!(error, StoreError::TaskRevisionConflict),
            Self::GoalRevision => matches!(error, StoreError::GoalRevisionConflict),
            Self::PolicyRevision => matches!(error, StoreError::PolicyRevisionConflict),
            Self::TransitionInvalid => matches!(error, StoreError::AgentTransitionInvalid(_)),
            Self::Terminal => matches!(error, StoreError::GoalAlreadyTerminal),
        }
    }
}

fn assertions_mut(transition: &mut RuntimeTransition) -> &mut Vec<CriterionAssertion> {
    let [command] = transition.commands.as_mut_slice() else {
        panic!("completion transition has one command")
    };
    let KernelCommandPayload::CompleteGoal(command) = &mut command.payload else {
        panic!("completion transition contains CompleteGoal")
    };
    &mut command.assertions
}

fn blocker_mut(transition: &mut RuntimeTransition) -> &mut kiteframe::BlockGoal {
    let [command] = transition.commands.as_mut_slice() else {
        panic!("block transition has one command")
    };
    let KernelCommandPayload::BlockGoal(command) = &mut command.payload else {
        panic!("block transition contains BlockGoal")
    };
    command
}

#[tokio::test]
async fn complete_goal_requires_exact_authorized_criterion_evidence() {
    let Some(fixture) = GoalFixture::three_harness_criteria().await else {
        return;
    };
    let before = fixture.current().await;
    let after = fixture.complete_three_criterion_goal().await.unwrap();
    assert_eq!(after.lifecycle, TaskLifecycle::Succeeded);
    assert_eq!(after.operational_status, OperationalStatus::Finished);
    assert_eq!(after.goal_phase, GoalPhase::Complete);
    assert_eq!(after.task_revision, before.task_revision + 3);
    assert_eq!(after.goal_revision, before.goal_revision + 1);
    assert_eq!(fixture.assertions().await, 3);
}

#[tokio::test]
async fn goal_completion_replay_and_conflict_preserve_exactly_once_journals() {
    let Some(fixture) = GoalFixture::three_harness_criteria().await else {
        return;
    };
    let mut prepared = fixture.prepare_three_criterion_completion().await;
    let evidence_id = match &prepared.completion.event {
        RuntimeEvent::HarnessResult { evidence_id, .. } => evidence_id.clone(),
        _ => unreachable!(),
    };
    let completed = fixture.commit_prepared(&prepared).await.unwrap();
    assert_eq!(completed.lifecycle, TaskLifecycle::Succeeded);
    let committed = fixture.database_fingerprint().await;
    assert_eq!(fixture.commit_prepared(&prepared).await.unwrap(), completed);
    assert_eq!(fixture.database_fingerprint().await, committed);

    if let RuntimeEvent::HarnessResult { fields, .. } = &mut prepared.completion.event {
        *fields = json!({"conflict": true});
    }
    assert!(matches!(
        fixture.commit_prepared(&prepared).await,
        Err(StoreError::IdempotencyConflict),
    ));
    assert_eq!(fixture.database_fingerprint().await, committed);

    let counts: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM goal_evidence WHERE task_id = $1), \
           (SELECT COUNT(*) FROM goal_criterion_assertions WHERE task_id = $1), \
           (SELECT COUNT(*) FROM goal_blockers WHERE task_id = $1), \
           (SELECT COUNT(*) FROM agent_transitions WHERE task_id = $1), \
           (SELECT COUNT(*) FROM agent_policy_decisions WHERE task_id = $1), \
           (SELECT COUNT(*) FROM agent_task_actions WHERE task_id = $1), \
           (SELECT COUNT(*) FROM agent_task_events WHERE task_id = $1)",
    )
    .bind(fixture.task_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(counts, (3, 3, 0, 4, 2, 1, 7));
    let (evidence_type, source_kind, source_identity, content_digest): (
        String,
        String,
        Uuid,
        String,
    ) = sqlx::query_as(
        "SELECT evidence_type, source_kind, source_identity, content_digest \
         FROM goal_evidence WHERE task_id = $1 AND id = $2",
    )
    .bind(fixture.task_id.as_uuid())
    .bind(evidence_id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(evidence_type, "harness.invocation_result");
    assert_eq!(source_kind, "builtin_harness");
    assert_eq!(source_identity, *prepared.completion.command_id.as_uuid());
    assert_eq!(
        content_digest,
        kiteframe::canonical_sha256(&json!({
            "outcome": "observed",
            "fields": {},
            "history": [],
        }))
        .unwrap(),
    );
}

#[tokio::test]
async fn every_invalid_completion_is_failure_atomic() {
    for (name, expected) in [
        ("missing criterion", ExpectedStoreError::Incomplete),
        ("extra criterion", ExpectedStoreError::Incomplete),
        ("duplicate criterion", ExpectedStoreError::Incomplete),
        ("same evidence reused", ExpectedStoreError::Incomplete),
        ("absent evidence", ExpectedStoreError::NotFound),
        ("foreign evidence", ExpectedStoreError::NotFound),
        ("wrong evidence type", ExpectedStoreError::TypeMismatch),
        ("unauthorized source", ExpectedStoreError::Unauthorized),
        ("failed source", ExpectedStoreError::Stale),
        ("changed source digest", ExpectedStoreError::Stale),
        ("evidence id collision", ExpectedStoreError::Idempotency),
        (
            "foreign evidence id collision",
            ExpectedStoreError::Idempotency,
        ),
        ("old task revision", ExpectedStoreError::TaskRevision),
        ("old goal revision", ExpectedStoreError::GoalRevision),
        ("denied policy", ExpectedStoreError::TransitionInvalid),
        (
            "changed policy revision",
            ExpectedStoreError::PolicyRevision,
        ),
        ("already complete", ExpectedStoreError::Terminal),
    ] {
        let Some(fixture) = GoalFixture::three_harness_criteria().await else {
            return;
        };
        let mut prepared = fixture.prepare_three_criterion_completion().await;
        let assertions = assertions_mut(&mut prepared.transition);
        match name {
            "missing criterion" => {
                assertions.pop();
            }
            "extra criterion" => assertions.push(CriterionAssertion {
                criterion_id: "unexpected".to_owned(),
                evidence_id: EvidenceId::new(Uuid::now_v7()),
            }),
            "duplicate criterion" => assertions.push(assertions[0].clone()),
            "same evidence reused" => {
                assertions[1].evidence_id = assertions[0].evidence_id.clone();
            }
            "absent evidence" => {
                assertions[0].evidence_id = EvidenceId::new(Uuid::now_v7());
            }
            "foreign evidence" => {
                let Some(foreign) = GoalFixture::three_harness_criteria().await else {
                    return;
                };
                let command = foreign.claim_next().await;
                let foreign_id = EvidenceId::new(Uuid::now_v7());
                foreign
                    .complete(&command, "proceed", json!({}), foreign_id.clone())
                    .await
                    .unwrap();
                assertions[0].evidence_id = foreign_id;
                foreign
                    .store
                    .close_session_and_cancel_work(foreign.session.id.clone(), &foreign.principal)
                    .await
                    .unwrap();
            }
            "wrong evidence type" => {
                let original = assertions[0].evidence_id.clone();
                let (source_identity, digest): (Uuid, String) = sqlx::query_as(
                    "SELECT source_identity, content_digest FROM goal_evidence \
                     WHERE task_id = $1 AND id = $2",
                )
                .bind(fixture.task_id.as_uuid())
                .bind(original.as_uuid())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
                let wrong = EvidenceId::new(Uuid::now_v7());
                sqlx::query(
                    "INSERT INTO goal_evidence \
                     (id, task_id, evidence_type, source_kind, source_identity, content_digest) \
                     VALUES ($1, $2, 'context.wrong_type', 'builtin_harness', $3, $4)",
                )
                .bind(wrong.as_uuid())
                .bind(fixture.task_id.as_uuid())
                .bind(source_identity)
                .bind(digest)
                .execute(&fixture.pool)
                .await
                .unwrap();
                assertions[0].evidence_id = wrong;
            }
            "unauthorized source" => {
                let source_id: Uuid = sqlx::query_scalar(
                    "SELECT source_identity FROM goal_evidence WHERE task_id = $1 AND id = $2",
                )
                .bind(fixture.task_id.as_uuid())
                .bind(assertions[0].evidence_id.as_uuid())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
                let payload = json!({"evidenceType": "context.external", "subject": {}});
                sqlx::query(
                    "UPDATE agent_commands SET command_kind = 'request_context', payload = $1, \
                     payload_digest = $2 WHERE id = $3",
                )
                .bind(&payload)
                .bind(kiteframe::canonical_sha256(&payload).unwrap())
                .bind(source_id)
                .execute(&fixture.pool)
                .await
                .unwrap();
            }
            "failed source" => {
                let source_id: Uuid = sqlx::query_scalar(
                    "SELECT source_identity FROM goal_evidence WHERE task_id = $1 AND id = $2",
                )
                .bind(fixture.task_id.as_uuid())
                .bind(assertions[0].evidence_id.as_uuid())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
                sqlx::query("UPDATE agent_commands SET state = 'failed' WHERE id = $1")
                    .bind(source_id)
                    .execute(&fixture.pool)
                    .await
                    .unwrap();
            }
            "changed source digest" => {
                let source_id: Uuid = sqlx::query_scalar(
                    "SELECT source_identity FROM goal_evidence WHERE task_id = $1 AND id = $2",
                )
                .bind(fixture.task_id.as_uuid())
                .bind(assertions[0].evidence_id.as_uuid())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
                sqlx::query(
                    "UPDATE agent_commands SET result = jsonb_set( \
                     result, '{payload,fields}', '{\"changed\":true}'::jsonb) WHERE id = $1",
                )
                .bind(source_id)
                .execute(&fixture.pool)
                .await
                .unwrap();
            }
            "evidence id collision" => {
                let collision: Uuid =
                    sqlx::query_scalar("SELECT id FROM goal_evidence WHERE task_id = $1 LIMIT 1")
                        .bind(fixture.task_id.as_uuid())
                        .fetch_one(&fixture.pool)
                        .await
                        .unwrap();
                let prospective = match &mut prepared.completion.event {
                    RuntimeEvent::HarnessResult { evidence_id, .. } => {
                        let original = evidence_id.clone();
                        *evidence_id = EvidenceId::new(collision);
                        original
                    }
                    _ => unreachable!(),
                };
                assertions_mut(&mut prepared.transition)
                    .iter_mut()
                    .find(|assertion| assertion.evidence_id == prospective)
                    .unwrap()
                    .evidence_id = EvidenceId::new(collision);
            }
            "foreign evidence id collision" => {
                let Some(foreign) = GoalFixture::three_harness_criteria().await else {
                    return;
                };
                let command = foreign.claim_next().await;
                let collision = EvidenceId::new(Uuid::now_v7());
                foreign
                    .complete(&command, "proceed", json!({}), collision.clone())
                    .await
                    .unwrap();
                foreign
                    .store
                    .close_session_and_cancel_work(foreign.session.id.clone(), &foreign.principal)
                    .await
                    .unwrap();
                let prospective = match &mut prepared.completion.event {
                    RuntimeEvent::HarnessResult { evidence_id, .. } => {
                        let original = evidence_id.clone();
                        *evidence_id = collision.clone();
                        original
                    }
                    _ => unreachable!(),
                };
                assertions_mut(&mut prepared.transition)
                    .iter_mut()
                    .find(|assertion| assertion.evidence_id == prospective)
                    .unwrap()
                    .evidence_id = collision;
            }
            "old task revision" => prepared.transition.expected_task_revision -= 1,
            "old goal revision" => prepared.transition.expected_goal_revision += 1,
            "denied policy" => prepared.policy.allowed = false,
            "changed policy revision" => {
                prepared.policy.policy_revision =
                    PolicyRevision::new(prepared.policy.policy_revision.get() + 1);
            }
            "already complete" => {
                fixture.commit_prepared(&prepared).await.unwrap();
                prepared.completion.idempotency_key =
                    IdempotencyKey::new(format!("complete-again-{}", Uuid::now_v7())).unwrap();
            }
            _ => unreachable!(),
        }
        let before = fixture.database_fingerprint().await;
        let error = fixture.commit_prepared(&prepared).await.unwrap_err();
        assert!(expected.matches(&error), "{name}: {error}");
        assert_eq!(fixture.database_fingerprint().await, before, "{name}");
        fixture
            .store
            .close_session_and_cancel_work(fixture.session.id.clone(), &fixture.principal)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn block_goal_binds_zero_based_contract_condition_and_history_is_immutable() {
    let Some(fixture) = GoalFixture::blocked_conditions().await else {
        return;
    };
    let before = fixture.current().await;
    let command = fixture.claim_next().await;
    let blocked = fixture
        .complete(
            &command,
            "no_path",
            json!({}),
            EvidenceId::new(Uuid::now_v7()),
        )
        .await
        .unwrap();
    assert_eq!(blocked.lifecycle, TaskLifecycle::Running);
    assert_eq!(blocked.goal_phase, GoalPhase::Blocked);
    assert_eq!(blocked.operational_status, OperationalStatus::Blocked);
    assert_eq!(blocked.task_revision, before.task_revision + 1);
    assert_eq!(blocked.goal_revision, before.goal_revision + 1);
    assert_eq!(blocked.blocker.as_ref().unwrap().blocked_when_index, 1);
    assert_eq!(fixture.blocker_history().await, 1);
}

#[tokio::test]
async fn every_invalid_blocker_is_failure_atomic() {
    for (name, expected) in [
        ("out of range index", ExpectedStoreError::TransitionInvalid),
        ("malformed reason", ExpectedStoreError::TransitionInvalid),
        ("blank explanation", ExpectedStoreError::TransitionInvalid),
        ("long explanation", ExpectedStoreError::TransitionInvalid),
        ("zero evidence", ExpectedStoreError::Incomplete),
        ("thirty three evidence", ExpectedStoreError::Incomplete),
        ("duplicate evidence", ExpectedStoreError::Incomplete),
        ("foreign evidence", ExpectedStoreError::NotFound),
        ("unauthorized evidence", ExpectedStoreError::Unauthorized),
        ("stale evidence", ExpectedStoreError::Stale),
        (
            "zero unlock conditions",
            ExpectedStoreError::TransitionInvalid,
        ),
        (
            "thirty three unlock conditions",
            ExpectedStoreError::TransitionInvalid,
        ),
        (
            "blank unlock condition",
            ExpectedStoreError::TransitionInvalid,
        ),
        (
            "long unlock condition",
            ExpectedStoreError::TransitionInvalid,
        ),
        ("old task revision", ExpectedStoreError::TaskRevision),
        ("old goal revision", ExpectedStoreError::GoalRevision),
        (
            "changed policy revision",
            ExpectedStoreError::PolicyRevision,
        ),
        ("denied policy", ExpectedStoreError::TransitionInvalid),
        ("paused goal", ExpectedStoreError::TransitionInvalid),
        ("complete goal", ExpectedStoreError::Terminal),
    ] {
        let Some(fixture) = GoalFixture::blocked_with_prior_evidence().await else {
            return;
        };
        let mut prepared = fixture.prepare_block_completion().await;
        match name {
            "out of range index" => blocker_mut(&mut prepared.transition).blocked_when_index = 2,
            "malformed reason" => {
                blocker_mut(&mut prepared.transition).reason_code = "Not Stable".to_owned();
            }
            "blank explanation" => {
                blocker_mut(&mut prepared.transition).explanation = " ".to_owned();
            }
            "long explanation" => {
                blocker_mut(&mut prepared.transition).explanation = "x".repeat(2_049);
            }
            "zero evidence" => blocker_mut(&mut prepared.transition).evidence_ids.clear(),
            "thirty three evidence" => {
                blocker_mut(&mut prepared.transition).evidence_ids =
                    (0..33).map(|_| EvidenceId::new(Uuid::now_v7())).collect();
            }
            "duplicate evidence" => {
                let evidence = blocker_mut(&mut prepared.transition).evidence_ids[0].clone();
                blocker_mut(&mut prepared.transition)
                    .evidence_ids
                    .push(evidence);
            }
            "foreign evidence" => {
                let Some(foreign) = GoalFixture::blocked_with_prior_evidence().await else {
                    return;
                };
                let command = foreign.claim_next().await;
                let evidence_id = EvidenceId::new(Uuid::now_v7());
                foreign
                    .complete(&command, "no_path", json!({}), evidence_id.clone())
                    .await
                    .unwrap();
                blocker_mut(&mut prepared.transition).evidence_ids = vec![evidence_id];
                foreign
                    .store
                    .close_session_and_cancel_work(foreign.session.id.clone(), &foreign.principal)
                    .await
                    .unwrap();
            }
            "unauthorized evidence" | "stale evidence" => {
                let prospective_id = match &prepared.completion.event {
                    RuntimeEvent::HarnessResult { evidence_id, .. } => evidence_id,
                    _ => unreachable!(),
                };
                let (evidence_id, source_id): (Uuid, Uuid) = sqlx::query_as(
                    "SELECT id, source_identity FROM goal_evidence \
                     WHERE task_id = $1 AND id <> $2 LIMIT 1",
                )
                .bind(fixture.task_id.as_uuid())
                .bind(prospective_id.as_uuid())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
                blocker_mut(&mut prepared.transition).evidence_ids =
                    vec![EvidenceId::new(evidence_id)];
                if name == "unauthorized evidence" {
                    let payload = json!({"evidenceType": "context.external", "subject": {}});
                    sqlx::query(
                        "UPDATE agent_commands SET command_kind = 'request_context', payload = $1, \
                         payload_digest = $2 WHERE id = $3",
                    )
                    .bind(&payload)
                    .bind(kiteframe::canonical_sha256(&payload).unwrap())
                    .bind(source_id)
                    .execute(&fixture.pool)
                    .await
                    .unwrap();
                } else {
                    sqlx::query("UPDATE agent_commands SET state = 'failed' WHERE id = $1")
                        .bind(source_id)
                        .execute(&fixture.pool)
                        .await
                        .unwrap();
                }
            }
            "zero unlock conditions" => {
                blocker_mut(&mut prepared.transition)
                    .unlock_conditions
                    .clear();
            }
            "thirty three unlock conditions" => {
                blocker_mut(&mut prepared.transition).unlock_conditions =
                    (0..33).map(|index| format!("condition {index}")).collect();
            }
            "blank unlock condition" => {
                blocker_mut(&mut prepared.transition).unlock_conditions = vec![" ".to_owned()];
            }
            "long unlock condition" => {
                blocker_mut(&mut prepared.transition).unlock_conditions = vec!["x".repeat(2_049)];
            }
            "old task revision" => prepared.transition.expected_task_revision -= 1,
            "old goal revision" => prepared.transition.expected_goal_revision += 1,
            "changed policy revision" => {
                prepared.policy.policy_revision =
                    PolicyRevision::new(prepared.policy.policy_revision.get() + 1);
            }
            "denied policy" => prepared.policy.allowed = false,
            "paused goal" => {
                sqlx::query("UPDATE goal_states SET phase = 'paused' WHERE task_id = $1")
                    .bind(fixture.task_id.as_uuid())
                    .execute(&fixture.pool)
                    .await
                    .unwrap();
                sqlx::query("UPDATE user_tasks SET operational_status = 'paused' WHERE id = $1")
                    .bind(fixture.task_id.as_uuid())
                    .execute(&fixture.pool)
                    .await
                    .unwrap();
            }
            "complete goal" => {
                sqlx::query("UPDATE goal_states SET phase = 'complete' WHERE task_id = $1")
                    .bind(fixture.task_id.as_uuid())
                    .execute(&fixture.pool)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = fixture.database_fingerprint().await;
        let error = fixture.commit_prepared(&prepared).await.unwrap_err();
        assert!(expected.matches(&error), "{name}: {error}");
        assert_eq!(fixture.database_fingerprint().await, before, "{name}");
        fixture
            .store
            .close_session_and_cancel_work(fixture.session.id.clone(), &fixture.principal)
            .await
            .unwrap();
    }
}

#[test]
fn goal_phase_edges_are_closed_and_complete_is_terminal() {
    let legal = std::collections::BTreeSet::from([
        (GoalPhase::Active, GoalPhase::Paused),
        (GoalPhase::Paused, GoalPhase::Active),
        (GoalPhase::Active, GoalPhase::Blocked),
        (GoalPhase::Blocked, GoalPhase::Active),
        (GoalPhase::Active, GoalPhase::Complete),
    ]);
    for from in [
        GoalPhase::Active,
        GoalPhase::Paused,
        GoalPhase::Blocked,
        GoalPhase::Complete,
    ] {
        for to in [
            GoalPhase::Active,
            GoalPhase::Paused,
            GoalPhase::Blocked,
            GoalPhase::Complete,
        ] {
            assert_eq!(
                goal_edge_allowed(from, to),
                legal.contains(&(from, to)),
                "{from:?} -> {to:?}",
            );
        }
    }
}

fn assert_exactly_one_race_winner(
    name: &str,
    first: &Result<AgentExecutionView, StoreError>,
    second: &Result<AgentExecutionView, StoreError>,
) {
    assert_eq!(
        usize::from(first.is_ok()) + usize::from(second.is_ok()),
        1,
        "{name}: first={first:?}, second={second:?}",
    );
    let error = first
        .as_ref()
        .err()
        .or_else(|| second.as_ref().err())
        .unwrap();
    assert!(
        matches!(
            error,
            StoreError::TaskRevisionConflict
                | StoreError::GoalRevisionConflict
                | StoreError::AgentTaskAlreadyTerminal
                | StoreError::GoalAlreadyTerminal
                | StoreError::AgentTransitionInvalid(_)
        ),
        "{name}: unexpected loser error {error}",
    );
}

#[tokio::test]
async fn lifecycle_authority_races_have_one_winner_and_no_deadlock() {
    let Some(two_pause) = GoalFixture::three_harness_criteria().await else {
        return;
    };
    let current = two_pause.current().await;
    let pause = |suffix: &str| PauseUserTask {
        session_id: two_pause.session.id.clone(),
        task_id: two_pause.task_id.clone(),
        expected_task_revision: current.task_revision,
        expected_goal_revision: current.goal_revision,
        reason: "Pause for an operator check.".to_owned(),
        idempotency_key: IdempotencyKey::new(format!("pause-{suffix}-{}", Uuid::now_v7())).unwrap(),
    };
    let two_pause_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            two_pause
                .kernel
                .pause_task(&two_pause.principal, pause("first")),
            two_pause
                .kernel
                .pause_task(&two_pause.principal, pause("second")),
        )
    })
    .await
    .expect("two pause requests must not deadlock");
    assert_exactly_one_race_winner("two pause keys", &two_pause_result.0, &two_pause_result.1);

    let Some(pause_completion) = GoalFixture::blocked_with_prior_evidence().await else {
        return;
    };
    let prepared_block = pause_completion.prepare_block_completion().await;
    let current = pause_completion.current().await;
    let pause_request = PauseUserTask {
        session_id: pause_completion.session.id.clone(),
        task_id: pause_completion.task_id.clone(),
        expected_task_revision: current.task_revision,
        expected_goal_revision: current.goal_revision,
        reason: "Pause before accepting the result.".to_owned(),
        idempotency_key: IdempotencyKey::new(format!("pause-race-{}", Uuid::now_v7())).unwrap(),
    };
    let pause_completion_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            pause_completion.commit_prepared(&prepared_block),
            pause_completion
                .kernel
                .pause_task(&pause_completion.principal, pause_request),
        )
    })
    .await
    .expect("pause and command completion must not deadlock");
    assert_exactly_one_race_winner(
        "pause versus command completion",
        &pause_completion_result.0,
        &pause_completion_result.1,
    );

    let Some(resume_cancel) = GoalFixture::blocked_conditions().await else {
        return;
    };
    let source = resume_cancel.claim_next().await;
    let blocked = resume_cancel
        .complete(
            &source,
            "no_path",
            json!({}),
            EvidenceId::new(Uuid::now_v7()),
        )
        .await
        .unwrap();
    let resume_request = ResumeUserTask {
        session_id: resume_cancel.session.id.clone(),
        task_id: resume_cancel.task_id.clone(),
        expected_task_revision: blocked.task_revision,
        expected_goal_revision: blocked.goal_revision,
        idempotency_key: IdempotencyKey::new(format!("resume-race-{}", Uuid::now_v7())).unwrap(),
    };
    let cancel_request = CancelUserTask {
        session_id: resume_cancel.session.id.clone(),
        task_id: resume_cancel.task_id.clone(),
        expected_task_revision: blocked.task_revision,
        expected_goal_revision: blocked.goal_revision,
        reason: Some("Cancel instead of resuming.".to_owned()),
        idempotency_key: IdempotencyKey::new(format!("cancel-race-{}", Uuid::now_v7())).unwrap(),
    };
    let resume_cancel_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            resume_cancel
                .kernel
                .resume_task(&resume_cancel.principal, resume_request),
            resume_cancel
                .kernel
                .cancel_task(&resume_cancel.principal, cancel_request),
        )
    })
    .await
    .expect("resume and cancel must not deadlock");
    assert_exactly_one_race_winner(
        "resume versus cancel",
        &resume_cancel_result.0,
        &resume_cancel_result.1,
    );

    let Some(completion_cancel) = GoalFixture::complete_or_block().await else {
        return;
    };
    let source = completion_cancel.claim_next().await;
    let prepared_complete = completion_cancel
        .prepare_claimed_authority(&source, "finished")
        .await;
    let current = completion_cancel.current().await;
    let cancel_request = CancelUserTask {
        session_id: completion_cancel.session.id.clone(),
        task_id: completion_cancel.task_id.clone(),
        expected_task_revision: current.task_revision,
        expected_goal_revision: current.goal_revision,
        reason: None,
        idempotency_key: IdempotencyKey::new(format!("cancel-complete-{}", Uuid::now_v7()))
            .unwrap(),
    };
    let completion_cancel_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            completion_cancel.commit_prepared(&prepared_complete),
            completion_cancel
                .kernel
                .cancel_task(&completion_cancel.principal, cancel_request),
        )
    })
    .await
    .expect("completion and cancel must not deadlock");
    assert_exactly_one_race_winner(
        "completion versus cancel",
        &completion_cancel_result.0,
        &completion_cancel_result.1,
    );

    let Some(block_complete) = GoalFixture::complete_or_block().await else {
        return;
    };
    let source = block_complete.claim_next().await;
    let prepared_complete = block_complete
        .prepare_claimed_authority(&source, "finished")
        .await;
    let prepared_block = block_complete
        .prepare_claimed_authority(&source, "no_path")
        .await;
    let block_complete_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            block_complete.commit_prepared(&prepared_block),
            block_complete.commit_prepared(&prepared_complete),
        )
    })
    .await
    .expect("block and complete must not deadlock");
    assert_exactly_one_race_winner(
        "block versus complete",
        &block_complete_result.0,
        &block_complete_result.1,
    );
}

#[tokio::test]
async fn principal_pause_resume_and_cancel_are_revisioned_and_exactly_idempotent() {
    let Some(fixture) = GoalFixture::three_harness_criteria().await else {
        return;
    };
    let initial = fixture.current().await;
    let pause = PauseUserTask {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        expected_task_revision: initial.task_revision,
        expected_goal_revision: initial.goal_revision,
        reason: "User requested a bounded pause.".to_owned(),
        idempotency_key: IdempotencyKey::new(format!("pause-{}", Uuid::now_v7())).unwrap(),
    };
    let paused = fixture
        .kernel
        .pause_task(&fixture.principal, pause.clone())
        .await
        .unwrap();
    assert_eq!(paused.lifecycle, TaskLifecycle::Running);
    assert_eq!(paused.operational_status, OperationalStatus::Paused);
    assert_eq!(paused.goal_phase, GoalPhase::Paused);
    assert_eq!(paused.task_revision, initial.task_revision + 1);
    assert_eq!(paused.goal_revision, initial.goal_revision + 1);
    assert_eq!(
        fixture
            .store
            .dispatchable_agent_commands(256)
            .await
            .unwrap()
            .into_iter()
            .filter(|command| command.task_id == fixture.task_id)
            .count(),
        0,
    );
    assert_eq!(
        fixture
            .kernel
            .pause_task(&fixture.principal, pause.clone())
            .await
            .unwrap()
            .task_revision,
        paused.task_revision,
    );
    let mut conflicting_pause = pause;
    conflicting_pause.reason = "A different reason under the same key.".to_owned();
    let before_conflict = fixture.database_fingerprint().await;
    assert!(matches!(
        fixture
            .kernel
            .pause_task(&fixture.principal, conflicting_pause)
            .await,
        Err(StoreError::IdempotencyConflict),
    ));
    assert_eq!(fixture.database_fingerprint().await, before_conflict);

    let resume = ResumeUserTask {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        expected_task_revision: paused.task_revision,
        expected_goal_revision: paused.goal_revision,
        idempotency_key: IdempotencyKey::new(format!("resume-{}", Uuid::now_v7())).unwrap(),
    };
    let resumed = fixture
        .kernel
        .resume_task(&fixture.principal, resume.clone())
        .await
        .unwrap();
    assert_eq!(
        resumed.operational_status,
        OperationalStatus::WaitingForHarness
    );
    assert_eq!(resumed.goal_phase, GoalPhase::Active);
    assert_eq!(resumed.task_revision, paused.task_revision + 1);
    assert_eq!(resumed.goal_revision, paused.goal_revision + 1);
    assert_eq!(
        fixture
            .kernel
            .resume_task(&fixture.principal, resume.clone())
            .await
            .unwrap()
            .task_revision,
        resumed.task_revision,
    );
    let mut conflicting_resume = resume;
    conflicting_resume.expected_task_revision += 1;
    let before_conflict = fixture.database_fingerprint().await;
    assert!(matches!(
        fixture
            .kernel
            .resume_task(&fixture.principal, conflicting_resume)
            .await,
        Err(StoreError::IdempotencyConflict),
    ));
    assert_eq!(fixture.database_fingerprint().await, before_conflict);

    let cancel = CancelUserTask {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        expected_task_revision: resumed.task_revision,
        expected_goal_revision: resumed.goal_revision,
        reason: Some("No longer needed.".to_owned()),
        idempotency_key: IdempotencyKey::new(format!("cancel-{}", Uuid::now_v7())).unwrap(),
    };
    let cancelled = fixture
        .kernel
        .cancel_task(&fixture.principal, cancel.clone())
        .await
        .unwrap();
    assert_eq!(cancelled.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(cancelled.operational_status, OperationalStatus::Finished);
    assert_eq!(cancelled.goal_phase, GoalPhase::Active);
    assert_eq!(cancelled.task_revision, resumed.task_revision + 1);
    assert_eq!(cancelled.goal_revision, resumed.goal_revision);
    assert_eq!(
        fixture
            .kernel
            .cancel_task(&fixture.principal, cancel.clone())
            .await
            .unwrap()
            .task_revision,
        cancelled.task_revision,
    );
    let mut conflicting_cancel = cancel;
    conflicting_cancel.reason = Some("Different cancellation content.".to_owned());
    let before_conflict = fixture.database_fingerprint().await;
    assert!(matches!(
        fixture
            .kernel
            .cancel_task(&fixture.principal, conflicting_cancel)
            .await,
        Err(StoreError::IdempotencyConflict),
    ));
    assert_eq!(fixture.database_fingerprint().await, before_conflict);
    assert_eq!(fixture.assertions().await, 0);
    assert_eq!(fixture.blocker_history().await, 0);
}

#[tokio::test]
async fn lifecycle_actions_reauthenticate_and_terminal_goal_rejects_all_actions() {
    let Some(fixture) = GoalFixture::three_harness_criteria().await else {
        return;
    };
    let current = fixture.current().await;
    let attacker = PrincipalId::new(format!("attacker-{}", Uuid::now_v7())).unwrap();
    let before = fixture.database_fingerprint().await;
    assert!(matches!(
        fixture
            .kernel
            .pause_task(
                &attacker,
                PauseUserTask {
                    session_id: fixture.session.id.clone(),
                    task_id: fixture.task_id.clone(),
                    expected_task_revision: current.task_revision,
                    expected_goal_revision: current.goal_revision,
                    reason: "Unauthorized pause.".to_owned(),
                    idempotency_key: IdempotencyKey::new(format!("pause-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await,
        Err(StoreError::PrincipalMismatch),
    ));
    assert!(matches!(
        fixture
            .kernel
            .resume_task(
                &attacker,
                ResumeUserTask {
                    session_id: fixture.session.id.clone(),
                    task_id: fixture.task_id.clone(),
                    expected_task_revision: current.task_revision,
                    expected_goal_revision: current.goal_revision,
                    idempotency_key: IdempotencyKey::new(format!("resume-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await,
        Err(StoreError::PrincipalMismatch),
    ));
    assert!(matches!(
        fixture
            .kernel
            .cancel_task(
                &attacker,
                CancelUserTask {
                    session_id: fixture.session.id.clone(),
                    task_id: fixture.task_id.clone(),
                    expected_task_revision: current.task_revision,
                    expected_goal_revision: current.goal_revision,
                    reason: None,
                    idempotency_key: IdempotencyKey::new(format!("cancel-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await,
        Err(StoreError::PrincipalMismatch),
    ));
    assert_eq!(fixture.database_fingerprint().await, before);

    assert!(matches!(
        fixture
            .kernel
            .pause_task(
                &fixture.principal,
                PauseUserTask {
                    session_id: fixture.session.id.clone(),
                    task_id: fixture.task_id.clone(),
                    expected_task_revision: current.task_revision,
                    expected_goal_revision: current.goal_revision,
                    reason: " ".to_owned(),
                    idempotency_key: IdempotencyKey::new(format!("pause-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await,
        Err(StoreError::AgentTransitionInvalid(_)),
    ));
    assert_eq!(fixture.database_fingerprint().await, before);

    let Some(terminal) = GoalFixture::complete_or_block().await else {
        return;
    };
    let source = terminal.claim_next().await;
    let completion = terminal
        .prepare_claimed_authority(&source, "finished")
        .await;
    let complete = terminal.commit_prepared(&completion).await.unwrap();
    let before = terminal.database_fingerprint().await;
    let pause = PauseUserTask {
        session_id: terminal.session.id.clone(),
        task_id: terminal.task_id.clone(),
        expected_task_revision: complete.task_revision,
        expected_goal_revision: complete.goal_revision,
        reason: "Too late.".to_owned(),
        idempotency_key: IdempotencyKey::new(format!("pause-{}", Uuid::now_v7())).unwrap(),
    };
    let resume = ResumeUserTask {
        session_id: terminal.session.id.clone(),
        task_id: terminal.task_id.clone(),
        expected_task_revision: complete.task_revision,
        expected_goal_revision: complete.goal_revision,
        idempotency_key: IdempotencyKey::new(format!("resume-{}", Uuid::now_v7())).unwrap(),
    };
    let cancel = CancelUserTask {
        session_id: terminal.session.id.clone(),
        task_id: terminal.task_id.clone(),
        expected_task_revision: complete.task_revision,
        expected_goal_revision: complete.goal_revision,
        reason: None,
        idempotency_key: IdempotencyKey::new(format!("cancel-{}", Uuid::now_v7())).unwrap(),
    };
    assert!(matches!(
        terminal.kernel.pause_task(&terminal.principal, pause).await,
        Err(StoreError::GoalAlreadyTerminal),
    ));
    assert!(matches!(
        terminal
            .kernel
            .resume_task(&terminal.principal, resume)
            .await,
        Err(StoreError::GoalAlreadyTerminal),
    ));
    assert!(matches!(
        terminal
            .kernel
            .cancel_task(&terminal.principal, cancel)
            .await,
        Err(StoreError::GoalAlreadyTerminal),
    ));
    assert_eq!(terminal.database_fingerprint().await, before);
}

#[tokio::test]
async fn accepted_task_can_be_cancelled_without_goal_inference() {
    let Some(fixture) = GoalFixture::accepted(
        r#"task inspect(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Inspect.""" tools []
    on finished() { done "finished" }
  }"#,
        vec![("finished", "The task is finished")],
        vec!["No policy-permitted path remains"],
    )
    .await
    else {
        return;
    };
    let accepted = fixture.current().await;
    assert_eq!(accepted.lifecycle, TaskLifecycle::Accepted);
    let cancelled = fixture
        .kernel
        .cancel_task(
            &fixture.principal,
            CancelUserTask {
                session_id: fixture.session.id.clone(),
                task_id: fixture.task_id.clone(),
                expected_task_revision: accepted.task_revision,
                expected_goal_revision: accepted.goal_revision,
                reason: Some("Cancelled before start.".to_owned()),
                idempotency_key: IdempotencyKey::new(format!("cancel-{}", Uuid::now_v7())).unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(cancelled.lifecycle, TaskLifecycle::Cancelled);
    assert_eq!(cancelled.goal_phase, GoalPhase::Active);
    assert_eq!(cancelled.goal_revision, accepted.goal_revision);
    assert!(cancelled.blocker.is_none());
    assert_eq!(fixture.assertions().await, 0);
    assert_eq!(fixture.blocker_history().await, 0);
}

#[tokio::test]
async fn blocked_resume_retains_history_and_proposes_fresh_work() {
    let Some(fixture) = GoalFixture::blocked_conditions().await else {
        return;
    };
    let command = fixture.claim_next().await;
    let blocked = fixture
        .complete(
            &command,
            "no_path",
            json!({}),
            EvidenceId::new(Uuid::now_v7()),
        )
        .await
        .unwrap();
    let resumed = fixture
        .kernel
        .resume_task(
            &fixture.principal,
            ResumeUserTask {
                session_id: fixture.session.id.clone(),
                task_id: fixture.task_id.clone(),
                expected_task_revision: blocked.task_revision,
                expected_goal_revision: blocked.goal_revision,
                idempotency_key: IdempotencyKey::new(format!("resume-{}", Uuid::now_v7())).unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(resumed.lifecycle, TaskLifecycle::Running);
    assert_eq!(resumed.goal_phase, GoalPhase::Active);
    assert_eq!(
        resumed.operational_status,
        OperationalStatus::WaitingForHarness
    );
    assert!(resumed.blocker.is_none());
    assert_eq!(fixture.blocker_history().await, 1);
    assert_eq!(resumed.task_revision, blocked.task_revision + 1);
    assert_eq!(resumed.goal_revision, blocked.goal_revision + 1);
    assert_eq!(
        fixture
            .store
            .dispatchable_agent_commands(256)
            .await
            .unwrap()
            .into_iter()
            .filter(|command| command.task_id == fixture.task_id)
            .count(),
        1,
    );
}

#[tokio::test]
async fn blocked_resume_authenticates_before_invoking_the_runtime() {
    let Some(fixture) = GoalFixture::blocked_conditions().await else {
        return;
    };
    let command = fixture.claim_next().await;
    let blocked = fixture
        .complete(
            &command,
            "no_path",
            json!({}),
            EvidenceId::new(Uuid::now_v7()),
        )
        .await
        .unwrap();
    let runtime = Arc::new(RecordingRuntime::new());
    let kernel = AgentKernel::new(
        Arc::clone(&fixture.store),
        runtime.clone(),
        Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&fixture.store))),
    );
    let request = ResumeUserTask {
        session_id: fixture.session.id.clone(),
        task_id: fixture.task_id.clone(),
        expected_task_revision: blocked.task_revision,
        expected_goal_revision: blocked.goal_revision,
        idempotency_key: IdempotencyKey::new(format!("resume-{}", Uuid::now_v7())).unwrap(),
    };
    let before_foreign = fixture.database_fingerprint().await;
    let foreign = PrincipalId::new(format!("foreign-{}", Uuid::now_v7())).unwrap();
    assert!(matches!(
        kernel.resume_task(&foreign, request.clone()).await,
        Err(StoreError::PrincipalMismatch),
    ));
    assert_eq!(runtime.handle_calls(), 0);
    assert_eq!(fixture.database_fingerprint().await, before_foreign);

    fixture
        .store
        .close_session_and_cancel_work(fixture.session.id.clone(), &fixture.principal)
        .await
        .unwrap();
    let before_closed = fixture.database_fingerprint().await;
    assert!(matches!(
        kernel.resume_task(&fixture.principal, request).await,
        Err(StoreError::SessionClosed),
    ));
    assert_eq!(runtime.handle_calls(), 0);
    assert_eq!(fixture.database_fingerprint().await, before_closed);
}

#[tokio::test]
async fn public_kernel_reports_terminal_authority_before_claim_revision_conflicts() {
    let Some(completed_fixture) = GoalFixture::complete_or_block().await else {
        return;
    };
    let source = completed_fixture.claim_next().await;
    let prepared = completed_fixture
        .prepare_claimed_authority(&source, "finished")
        .await;
    let completed = completed_fixture
        .kernel
        .complete_command(&completed_fixture.principal, prepared.completion.clone())
        .await
        .unwrap();
    assert_eq!(completed.goal_phase, GoalPhase::Complete);
    assert_eq!(
        completed_fixture
            .kernel
            .complete_command(&completed_fixture.principal, prepared.completion.clone(),)
            .await
            .unwrap(),
        completed,
    );
    let completed_fingerprint = completed_fixture.database_fingerprint().await;

    let mut new_complete = prepared.completion.clone();
    new_complete.idempotency_key =
        IdempotencyKey::new(format!("complete-{}", Uuid::now_v7())).unwrap();
    assert!(matches!(
        completed_fixture
            .kernel
            .complete_command(&completed_fixture.principal, new_complete)
            .await,
        Err(StoreError::GoalAlreadyTerminal),
    ));

    let mut new_block = prepared.completion;
    new_block.idempotency_key = IdempotencyKey::new(format!("block-{}", Uuid::now_v7())).unwrap();
    let RuntimeEvent::HarnessResult {
        event_id,
        outcome,
        evidence_id,
        ..
    } = &mut new_block.event
    else {
        unreachable!()
    };
    *event_id = format!("event-{}", Uuid::now_v7());
    *outcome = "no_path".to_owned();
    *evidence_id = EvidenceId::new(Uuid::now_v7());
    assert!(matches!(
        completed_fixture
            .kernel
            .complete_command(&completed_fixture.principal, new_block)
            .await,
        Err(StoreError::GoalAlreadyTerminal),
    ));
    assert_eq!(
        completed_fixture.database_fingerprint().await,
        completed_fingerprint,
    );

    let Some(failed_fixture) = GoalFixture::complete_or_block().await else {
        return;
    };
    let source = failed_fixture.claim_next().await;
    let failure = FailAgentCommand {
        session_id: failed_fixture.session.id.clone(),
        task_id: failed_fixture.task_id.clone(),
        command_id: source.id,
        expected_command_revision: source.revision,
        idempotency_key: IdempotencyKey::new(format!("fail-{}", Uuid::now_v7())).unwrap(),
        code: "harness_failed".to_owned(),
    };
    let failed = failed_fixture
        .kernel
        .fail_command(&failed_fixture.principal, failure.clone())
        .await
        .unwrap();
    assert_eq!(failed.lifecycle, TaskLifecycle::Failed);
    assert_eq!(
        failed_fixture
            .kernel
            .fail_command(&failed_fixture.principal, failure.clone())
            .await
            .unwrap(),
        failed,
    );
    let failed_fingerprint = failed_fixture.database_fingerprint().await;
    let mut new_failure = failure;
    new_failure.idempotency_key = IdempotencyKey::new(format!("fail-{}", Uuid::now_v7())).unwrap();
    assert!(matches!(
        failed_fixture
            .kernel
            .fail_command(&failed_fixture.principal, new_failure)
            .await,
        Err(StoreError::AgentTaskAlreadyTerminal),
    ));
    assert_eq!(
        failed_fixture.database_fingerprint().await,
        failed_fingerprint
    );
}

#[tokio::test]
async fn blocked_resume_hashes_unbounded_external_keys_into_stable_internal_event_ids() {
    for (key, expected_digest) in [
        (
            "a".repeat(67),
            "6116c09f89718e829f69b7afb1d9d60c6973935d96b33213e4d28689fe108ee7",
        ),
        (
            "b".repeat(68),
            "97fdd2ff4b4a2448a99045bdb1986d021275a13b91cc6cd09614f5a40fbeca42",
        ),
        (
            "long".repeat(1_000),
            "59f1d666fce5d94e6a3225cb92ad724a95e43d13b9621de98f443e730a346105",
        ),
        (
            "界".repeat(100),
            "57c42d71b2a0f0d14086964cf358b71a2f29b17ff54f0d4801a6d344c215544d",
        ),
    ] {
        let Some(fixture) = GoalFixture::blocked_conditions().await else {
            return;
        };
        let command = fixture.claim_next().await;
        let blocked = fixture
            .complete(
                &command,
                "no_path",
                json!({}),
                EvidenceId::new(Uuid::now_v7()),
            )
            .await
            .unwrap();
        let request = ResumeUserTask {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            expected_task_revision: blocked.task_revision,
            expected_goal_revision: blocked.goal_revision,
            idempotency_key: IdempotencyKey::new(key.clone()).unwrap(),
        };
        let resumed = fixture
            .kernel
            .resume_task(&fixture.principal, request.clone())
            .await
            .unwrap();
        assert_eq!(resumed.goal_phase, GoalPhase::Active);
        assert_eq!(
            fixture
                .kernel
                .resume_task(&fixture.principal, request.clone())
                .await
                .unwrap(),
            resumed,
        );

        let stored_external_key: String = sqlx::query_scalar(
            "SELECT idempotency_key FROM agent_task_actions \
             WHERE task_id = $1 AND action_kind = 'resume'",
        )
        .bind(fixture.task_id.as_uuid())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(stored_external_key, key);
        let (source_event_id, transition_key): (String, String) = sqlx::query_as(
            "SELECT source_event_id, idempotency_key FROM agent_transitions \
             WHERE task_id = $1 AND source_event_id LIKE 'principal-resumed:%'",
        )
        .bind(fixture.task_id.as_uuid())
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        let expected_event_id = format!(
            "principal-resumed:{}:{expected_digest}",
            fixture.task_id.as_uuid(),
        );
        assert_eq!(source_event_id, expected_event_id);
        assert_eq!(transition_key, format!("event:{expected_event_id}"));
        assert!(transition_key.len() <= 128);

        let before_conflict = fixture.database_fingerprint().await;
        let mut conflicting = request;
        conflicting.expected_task_revision += 1;
        assert!(matches!(
            fixture
                .kernel
                .resume_task(&fixture.principal, conflicting)
                .await,
            Err(StoreError::IdempotencyConflict),
        ));
        assert_eq!(fixture.database_fingerprint().await, before_conflict);
    }
}

#[test]
fn runtime_cannot_impersonate_principal_lifecycle_authority() {
    let types = std::fs::read_to_string("src/agent/types.rs").unwrap();
    let command_block = types
        .split("pub enum KernelCommandPayload")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    for forbidden in ["Pause", "Resume", "Cancel"] {
        assert!(!command_block.contains(forbidden));
    }
    for path in ["src/agent/kite.rs", "src/store/agent.rs", "src/task.rs"] {
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("RuntimeEvent::PrincipalResumed { event_id:"),
            "{path} must not impersonate authenticated resume",
        );
    }
}
