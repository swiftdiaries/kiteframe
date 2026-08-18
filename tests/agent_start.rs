use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use kiteframe::dsl::{CompiledDefinition, Limits, compile};
use kiteframe::{
    AcceptedUserTask, AgentExecutionView, AgentKernel, AgentLifecycleAction,
    AgentLifecycleAuthorizer, AgentPolicyDecision, AgentRuntime, AgentRuntimeError, AgentSelector,
    AgentStartCommit, CatalogTool, ClosedSchema, CreateSession, EffectKind, GoalContract,
    GoalPhase, IdempotencyKey, KernelCommand, KernelCommandPayload, OperationalStatus,
    PolicyRevision, PolicyUpdate, PostgresStore, PrincipalId, PublishedDefinition, RecoverySupport,
    RequestContext, RuntimeCeilings, RuntimeCommandKey, RuntimeDisposition, RuntimeStart,
    RuntimeStateSnapshot, RuntimeTransition, SessionView, StartAgent, StoreError, StoreLimits,
    SubmitUserTask, SuccessCriterion, TaskLifecycle, TaskOwnerAuthorizer, TaskPreferences,
    TaskTarget, ToolCatalogSnapshot,
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tokio::sync::Barrier;
use uuid::Uuid;

struct StartFixture {
    kernel: Arc<AgentKernel>,
    runtime: Arc<RecordingRuntime>,
    pool: PgPool,
    session: SessionView,
    principal: PrincipalId,
    accepted: AcceptedUserTask,
    store: Arc<PostgresStore>,
    catalog: ToolCatalogSnapshot,
}
impl StartFixture {
    async fn accepted_version_one() -> Option<Self> {
        Self::accepted_with_limits(StoreLimits::default()).await
    }

    async fn accepted_with_limits(limits: StoreLimits) -> Option<Self> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let store = Arc::new(
            PostgresStore::connect_with_limits(&database_url, limits)
                .await
                .unwrap(),
        );
        let principal = PrincipalId::new(format!("agent-start-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/agent-start".to_owned(),
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
        let definition = compiled_agent(&catalog, None);
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
        let runtime = Arc::new(RecordingRuntime::default());
        let authorizer = Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&store)));
        let kernel = Arc::new(AgentKernel::new(
            Arc::clone(&store),
            runtime.clone(),
            authorizer,
        ));
        Some(Self {
            kernel,
            runtime,
            pool: store.pool().clone(),
            session,
            principal,
            accepted,
            store,
            catalog,
        })
    }

    async fn publish_changed_version(&self) -> PublishedDefinition {
        let definition = compiled_agent(&self.catalog, Some(&self.accepted.definition.name));
        self.store
            .publish(self.session.id.clone(), &definition, &definition.source)
            .await
            .unwrap()
    }

    fn start_request(&self, key: &str) -> StartAgent {
        StartAgent {
            session_id: self.session.id.clone(),
            task_id: self.accepted.id.clone(),
            expected_task_revision: 0,
            expected_goal_revision: 0,
            idempotency_key: IdempotencyKey::new(key).unwrap(),
        }
    }

    async fn count(&self, table: &str) -> i64 {
        let query = format!("SELECT COUNT(*) FROM {table} WHERE task_id = $1",);
        sqlx::query_scalar(&query)
            .bind(self.accepted.id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn current(&self) -> AgentExecutionView {
        self.store
            .get_agent_execution(self.session.id.clone(), self.accepted.id.clone())
            .await
            .unwrap()
    }

    async fn reject_agent_command_insert(&self) {
        let suffix = self.accepted.id.as_uuid().simple();
        let function_name = format!("reject_agent_command_{suffix}");
        let trigger_name = format!("reject_agent_command_insert_{suffix}");
        sqlx::query(&format!(
            "CREATE FUNCTION {function_name}() RETURNS trigger AS $$ \
             BEGIN RAISE EXCEPTION 'test command rejection' USING ERRCODE = '55000'; END; \
             $$ LANGUAGE plpgsql"
        ))
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE TRIGGER {trigger_name} BEFORE INSERT ON agent_commands \
             FOR EACH ROW WHEN (NEW.task_id = '{}'::uuid) \
             EXECUTE FUNCTION {function_name}()",
            self.accepted.id.as_uuid()
        ))
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

#[derive(Default)]
struct RecordingRuntime {
    validated_hashes: Mutex<Vec<String>>,
    starts: Mutex<u64>,
}

impl RecordingRuntime {
    fn validated_hashes(&self) -> Vec<String> {
        self.validated_hashes.lock().unwrap().clone()
    }

    fn start_count(&self) -> u64 {
        *self.starts.lock().unwrap()
    }
}

impl AgentRuntime for RecordingRuntime {
    fn validate_agent(&self, definition: &CompiledDefinition) -> Result<(), AgentRuntimeError> {
        self.validated_hashes
            .lock()
            .unwrap()
            .push(definition.definition_hash.clone());
        Ok(())
    }

    fn start(&self, input: RuntimeStart<'_>) -> Result<RuntimeTransition, AgentRuntimeError> {
        *self.starts.lock().unwrap() += 1;
        Ok(RuntimeTransition {
            idempotency_key: "runtime-start".to_owned(),
            expected_task_revision: input.execution.task_revision,
            expected_goal_revision: input.execution.goal_revision,
            snapshot: RuntimeStateSnapshot::new(
                RuntimeDisposition::Running,
                json!({"definitionHash": input.definition.definition_hash}),
                &RuntimeCeilings::default(),
            )?,
            commands: vec![KernelCommand {
                schema_version: 1,
                key: RuntimeCommandKey::new("context-1")?,
                payload: KernelCommandPayload::RequestContext(RequestContext {
                    evidence_type: "github.review_thread".to_owned(),
                    subject: json!({"repository": "acme/widgets", "number": 142}),
                }),
            }],
        })
    }

    fn handle(
        &self,
        _input: kiteframe::RuntimeHandle<'_>,
    ) -> Result<RuntimeTransition, AgentRuntimeError> {
        unreachable!("start tests do not handle runtime events")
    }
}

struct PausingAuthorizer {
    store: Arc<PostgresStore>,
    authorized: Arc<Barrier>,
    release: Arc<Barrier>,
}

#[async_trait::async_trait]
impl AgentLifecycleAuthorizer for PausingAuthorizer {
    async fn authorize(
        &self,
        principal: &PrincipalId,
        task: &AcceptedUserTask,
        _action: AgentLifecycleAction,
    ) -> Result<AgentPolicyDecision, StoreError> {
        let policy_revision = self.store.policy_revision(task.session_id.clone()).await?;
        self.authorized.wait().await;
        self.release.wait().await;
        Ok(AgentPolicyDecision {
            allowed: principal == &task.principal,
            policy_revision,
            reason_code: "authenticated_task_owner".to_owned(),
        })
    }
}

#[tokio::test]
async fn start_uses_the_frozen_definition_after_a_newer_publication() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let newer = fixture.publish_changed_version().await;
    assert!(newer.version > fixture.accepted.definition.version);

    let started = fixture
        .kernel
        .start_agent(
            &fixture.principal,
            StartAgent {
                session_id: fixture.session.id.clone(),
                task_id: fixture.accepted.id.clone(),
                expected_task_revision: 0,
                expected_goal_revision: 0,
                idempotency_key: IdempotencyKey::new("start-frozen-v1").unwrap(),
            },
        )
        .await
        .unwrap();

    assert_eq!(started.task.definition.id, fixture.accepted.definition.id);
    assert_eq!(started.task.definition.version, 1);
    assert_eq!(
        fixture.runtime.validated_hashes(),
        vec![fixture.accepted.definition.definition_hash.clone()]
    );
}

#[tokio::test]
async fn concurrent_identical_start_commits_one_snapshot_command_and_event() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let request = fixture.start_request("same-start");
    let barrier = Arc::new(Barrier::new(3));
    let launch = |barrier: Arc<Barrier>| {
        let kernel = fixture.kernel.clone();
        let principal = fixture.principal.clone();
        let request = request.clone();
        async move {
            barrier.wait().await;
            kernel.start_agent(&principal, request).await
        }
    };
    let first = tokio::spawn(launch(barrier.clone()));
    let second = tokio::spawn(launch(barrier.clone()));
    barrier.wait().await;
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap().unwrap();
    let second = second.unwrap().unwrap();
    assert_eq!(first.task.id, second.task.id);
    assert_eq!(fixture.count("agent_runtime_snapshots").await, 1);
    assert_eq!(fixture.count("agent_transitions").await, 1);
    assert_eq!(fixture.count("agent_commands").await, 1);
    assert_eq!(fixture.count("agent_policy_decisions").await, 1);
    assert_eq!(fixture.count("agent_task_events").await, 1);
}

#[tokio::test]
async fn exact_start_replay_returns_the_current_execution_without_reinvoking_runtime() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let request = fixture.start_request("exact-replay");
    let first = fixture
        .kernel
        .start_agent(&fixture.principal, request.clone())
        .await
        .unwrap();
    let second = fixture
        .kernel
        .start_agent(&fixture.principal, request)
        .await
        .unwrap();

    assert_eq!(second, first);
    assert_eq!(fixture.runtime.start_count(), 1);
    assert_eq!(fixture.count("agent_task_actions").await, 1);
}

#[tokio::test]
async fn changed_request_content_under_the_same_start_key_conflicts() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let request = fixture.start_request("conflicting-replay");
    fixture
        .kernel
        .start_agent(&fixture.principal, request.clone())
        .await
        .unwrap();
    let mut changed = request;
    changed.expected_task_revision = 1;

    assert!(matches!(
        fixture
            .kernel
            .start_agent(&fixture.principal, changed)
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(fixture.runtime.start_count(), 1);
}

#[tokio::test]
async fn task_owner_authorizer_has_no_non_owner_allow_fallback() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let stranger = PrincipalId::new(format!("stranger-{}", Uuid::now_v7())).unwrap();
    let decision = TaskOwnerAuthorizer::new(Arc::clone(&fixture.store))
        .authorize(&stranger, &fixture.accepted, AgentLifecycleAction::Start)
        .await
        .unwrap();
    assert_eq!(
        decision,
        AgentPolicyDecision {
            allowed: false,
            policy_revision: PolicyRevision::new(0),
            reason_code: "authenticated_task_owner".to_owned(),
        }
    );
    assert!(matches!(
        fixture
            .kernel
            .start_agent(&stranger, fixture.start_request("unauthorized-start"))
            .await,
        Err(StoreError::PrincipalMismatch)
    ));
    assert_eq!(fixture.runtime.start_count(), 0);
    assert_eq!(fixture.count("agent_task_actions").await, 0);
}

#[tokio::test]
async fn a_fresh_key_cannot_start_an_execution_that_already_has_runtime_state() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    fixture
        .kernel
        .start_agent(&fixture.principal, fixture.start_request("first-start"))
        .await
        .unwrap();

    assert!(matches!(
        fixture
            .kernel
            .start_agent(&fixture.principal, fixture.start_request("fresh-start"))
            .await,
        Err(StoreError::AgentTaskAlreadyTerminal)
    ));
    assert_eq!(fixture.runtime.start_count(), 1);
}

#[tokio::test]
async fn policy_revision_change_between_authorization_and_commit_is_atomic() {
    let Some(mut fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let authorized = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    fixture.kernel = Arc::new(AgentKernel::new(
        Arc::clone(&fixture.store),
        fixture.runtime.clone(),
        Arc::new(PausingAuthorizer {
            store: Arc::clone(&fixture.store),
            authorized: Arc::clone(&authorized),
            release: Arc::clone(&release),
        }),
    ));
    let kernel = fixture.kernel.clone();
    let principal = fixture.principal.clone();
    let request = fixture.start_request("policy-race");
    let start = tokio::spawn(async move { kernel.start_agent(&principal, request).await });
    authorized.wait().await;
    fixture
        .store
        .append_policy_rules(PolicyUpdate {
            session: fixture.session.id.clone(),
            expected_revision: PolicyRevision::new(0),
            idempotency_key: format!("policy-{}", Uuid::now_v7()),
            typed_source: json!({"version": 1, "rules": []}),
            compiled_cedar: String::new(),
        })
        .await
        .unwrap();
    release.wait().await;

    assert!(matches!(
        start.await.unwrap(),
        Err(StoreError::PolicyRevisionConflict)
    ));
    let current = fixture.current().await;
    assert_eq!(current.lifecycle, TaskLifecycle::Accepted);
    assert_eq!(current.operational_status, OperationalStatus::NotStarted);
    assert_eq!((current.task_revision, current.goal_revision), (0, 0));
    assert!(current.runtime.is_none());
    for table in [
        "agent_runtime_snapshots",
        "agent_transitions",
        "agent_commands",
        "agent_policy_decisions",
        "agent_task_events",
        "agent_task_actions",
    ] {
        assert_eq!(fixture.count(table).await, 0, "unexpected row in {table}");
    }
}

#[tokio::test]
async fn command_insert_failure_rolls_back_the_entire_start() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    fixture.reject_agent_command_insert().await;
    assert!(
        fixture
            .kernel
            .start_agent(&fixture.principal, fixture.start_request("atomic-start"))
            .await
            .is_err()
    );
    let view = fixture.current().await;
    assert_eq!(view.lifecycle, TaskLifecycle::Accepted);
    assert_eq!(view.goal_phase, GoalPhase::Active);
    assert_eq!((view.task_revision, view.goal_revision), (0, 0));
    assert!(view.runtime.is_none());
    for table in [
        "agent_runtime_snapshots",
        "agent_transitions",
        "agent_commands",
        "agent_policy_decisions",
        "agent_task_events",
        "agent_task_actions",
    ] {
        assert_eq!(fixture.count(table).await, 0, "unexpected row in {table}");
    }
}

#[tokio::test]
async fn caller_cannot_override_deployment_ceilings_or_commit_partial_start_state() {
    let deployment_ceilings = RuntimeCeilings {
        max_snapshot_bytes: 1,
        ..RuntimeCeilings::default()
    };
    let Some(fixture) = StartFixture::accepted_with_limits(StoreLimits {
        agent_runtime: deployment_ceilings,
        ..StoreLimits::default()
    })
    .await
    else {
        return;
    };
    let request = fixture.start_request("caller-ceiling-override");
    let seed = fixture
        .store
        .load_agent_start_seed(fixture.session.id.clone(), fixture.accepted.id.clone())
        .await
        .unwrap();
    let policy = TaskOwnerAuthorizer::new(Arc::clone(&fixture.store))
        .authorize(
            &fixture.principal,
            &fixture.accepted,
            AgentLifecycleAction::Start,
        )
        .await
        .unwrap();
    let transition = fixture
        .runtime
        .start(RuntimeStart {
            execution: &seed.execution,
            definition: &seed.definition,
        })
        .unwrap();
    assert!(transition.snapshot.canonical_byte_count > 1);
    let caller_ceilings = RuntimeCeilings::default();

    assert!(matches!(
        fixture
            .store
            .commit_agent_start(AgentStartCommit {
                principal: &fixture.principal,
                request: &request,
                frozen_definition_id: fixture.accepted.definition.id,
                frozen_definition_hash: &fixture.accepted.definition.definition_hash,
                policy: &policy,
                transition: &transition,
                ceilings: &caller_ceilings,
            })
            .await,
        Err(StoreError::AgentTransitionInvalid(message))
            if message == "runtime ceilings do not match deployment configuration"
    ));
    let task: (String, i64) =
        sqlx::query_as("SELECT state, revision FROM user_tasks WHERE id = $1")
            .bind(fixture.accepted.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let goal_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM goal_states WHERE task_id = $1")
            .bind(fixture.accepted.id.as_uuid())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(task, ("accepted".to_owned(), 0));
    assert_eq!(goal_revision, 0);
    for table in [
        "agent_runtime_snapshots",
        "agent_transitions",
        "agent_commands",
        "agent_policy_decisions",
        "agent_task_events",
        "agent_task_actions",
    ] {
        assert_eq!(fixture.count(table).await, 0, "unexpected row in {table}");
    }
}

#[tokio::test]
async fn start_persists_the_exact_policy_and_runtime_receipt() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let started = fixture
        .kernel
        .start_agent(&fixture.principal, fixture.start_request("persisted-start"))
        .await
        .unwrap();
    assert_eq!(started.lifecycle, TaskLifecycle::Running);
    assert_eq!(
        started.operational_status,
        OperationalStatus::WaitingForContext
    );
    assert_eq!((started.task_revision, started.goal_revision), (1, 0));
    let policy: (bool, i64, String) = sqlx::query_as(
        "SELECT allowed, policy_revision, reason_code FROM agent_policy_decisions \
         WHERE task_id = $1",
    )
    .bind(fixture.accepted.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(policy, (true, 0, "authenticated_task_owner".to_owned()));
    let command: (String, serde_json::Value, String) = sqlx::query_as(
        "SELECT command_kind, payload, payload_digest FROM agent_commands WHERE task_id = $1",
    )
    .bind(fixture.accepted.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let expected_payload = json!({
        "evidenceType": "github.review_thread",
        "subject": {"repository": "acme/widgets", "number": 142}
    });
    assert_eq!(command.0, "request_context");
    assert_eq!(command.1, expected_payload);
    assert_eq!(
        command.2,
        kiteframe::canonical_sha256(&expected_payload).unwrap()
    );
    let event: (String, serde_json::Value, i64, i64) = sqlx::query_as(
        "SELECT kind, metadata, task_revision, goal_revision \
         FROM agent_task_events WHERE task_id = $1",
    )
    .bind(fixture.accepted.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(event.0, "agent.started");
    assert_eq!(event.2, 1);
    assert_eq!(event.3, 0);
    assert_eq!(
        event.1["definition_id"],
        fixture.accepted.definition.id.to_string()
    );
    assert_eq!(
        event.1["definition_hash"],
        fixture.accepted.definition.definition_hash
    );
    assert_eq!(event.1["policy_revision"], 0);
    assert_eq!(event.1["reason_code"], "authenticated_task_owner");
    assert_eq!(event.1["command_count"], 1);
    assert_eq!(
        event.1["snapshot_digest"],
        started.runtime.unwrap().snapshot_digest
    );
}

#[tokio::test]
async fn commit_rejects_a_seed_that_does_not_match_the_locked_definition() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let request = fixture.start_request("frozen-commit");
    let seed = fixture
        .store
        .load_agent_start_seed(fixture.session.id.clone(), fixture.accepted.id.clone())
        .await
        .unwrap();
    let policy = TaskOwnerAuthorizer::new(Arc::clone(&fixture.store))
        .authorize(
            &fixture.principal,
            &fixture.accepted,
            AgentLifecycleAction::Start,
        )
        .await
        .unwrap();
    let transition = fixture
        .runtime
        .start(RuntimeStart {
            execution: &seed.execution,
            definition: &seed.definition,
        })
        .unwrap();

    assert!(matches!(
        fixture
            .store
            .commit_agent_start(AgentStartCommit {
                principal: &fixture.principal,
                request: &request,
                frozen_definition_id: Uuid::now_v7(),
                frozen_definition_hash: &fixture.accepted.definition.definition_hash,
                policy: &policy,
                transition: &transition,
                ceilings: &RuntimeCeilings::default(),
            })
            .await,
        Err(StoreError::AgentTransitionInvalid(_))
    ));
    assert!(matches!(
        fixture
            .store
            .commit_agent_start(AgentStartCommit {
                principal: &fixture.principal,
                request: &request,
                frozen_definition_id: fixture.accepted.definition.id,
                frozen_definition_hash: "sha256:not-the-frozen-definition",
                policy: &policy,
                transition: &transition,
                ceilings: &RuntimeCeilings::default(),
            })
            .await,
        Err(StoreError::AgentTransitionInvalid(_))
    ));
    assert_eq!(fixture.count("agent_task_actions").await, 0);
}

#[tokio::test]
async fn exact_receipt_replays_even_if_a_retried_runtime_output_differs() {
    let Some(fixture) = StartFixture::accepted_version_one().await else {
        return;
    };
    let request = fixture.start_request("runtime-output-replay");
    let seed = fixture
        .store
        .load_agent_start_seed(fixture.session.id.clone(), fixture.accepted.id.clone())
        .await
        .unwrap();
    let policy = TaskOwnerAuthorizer::new(Arc::clone(&fixture.store))
        .authorize(
            &fixture.principal,
            &fixture.accepted,
            AgentLifecycleAction::Start,
        )
        .await
        .unwrap();
    let first_transition = fixture
        .runtime
        .start(RuntimeStart {
            execution: &seed.execution,
            definition: &seed.definition,
        })
        .unwrap();
    let first = fixture
        .store
        .commit_agent_start(AgentStartCommit {
            principal: &fixture.principal,
            request: &request,
            frozen_definition_id: fixture.accepted.definition.id,
            frozen_definition_hash: &fixture.accepted.definition.definition_hash,
            policy: &policy,
            transition: &first_transition,
            ceilings: &RuntimeCeilings::default(),
        })
        .await
        .unwrap();
    let changed_transition = RuntimeTransition {
        idempotency_key: "changed-runtime-start".to_owned(),
        expected_task_revision: 0,
        expected_goal_revision: 0,
        snapshot: RuntimeStateSnapshot::new(
            RuntimeDisposition::Running,
            json!({"differentSnapshot": true}),
            &RuntimeCeilings::default(),
        )
        .unwrap(),
        commands: vec![KernelCommand {
            schema_version: 1,
            key: RuntimeCommandKey::new("different-context").unwrap(),
            payload: KernelCommandPayload::RequestContext(RequestContext {
                evidence_type: "github.review_thread".to_owned(),
                subject: json!({"differentPayload": true}),
            }),
        }],
    };
    let replay = fixture
        .store
        .commit_agent_start(AgentStartCommit {
            principal: &fixture.principal,
            request: &request,
            frozen_definition_id: Uuid::now_v7(),
            frozen_definition_hash: "sha256:stale-seed",
            policy: &policy,
            transition: &changed_transition,
            ceilings: &RuntimeCeilings::default(),
        })
        .await
        .unwrap();

    assert_eq!(replay, first);
    assert_eq!(fixture.count("agent_runtime_snapshots").await, 1);
    assert_eq!(fixture.count("agent_transitions").await, 1);
    assert_eq!(fixture.count("agent_commands").await, 1);
}

fn compiled_agent(
    catalog: &ToolCatalogSnapshot,
    existing_name: Option<&str>,
) -> CompiledDefinition {
    let name = existing_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("start_agent_{}", Uuid::now_v7().simple()));
    let prompt = if existing_name.is_some() {
        "Inspect the pull request with refreshed evidence."
    } else {
        "Inspect the pull request."
    };
    let source = format!(
        r#"agent {name}(repository: string) -> string {{
  entry inspect(repository = repository)
  events ["github.pull_request.changed.v1"]
  limits {{ steps: 20 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task inspect(repository: string) -> string {{
    model reasoning
    prompt """{prompt}"""
    tools [search]
    on finished(summary: string) {{ done summary }}
  }}
}}"#
    );
    compile(&source, catalog, &Limits::default()).unwrap()
}
