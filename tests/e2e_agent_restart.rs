use std::sync::Arc;

use kiteframe::dsl::{Limits, compile};
use kiteframe::harness::{
    DenyAllHarnessApprovals, HarnessInvocationId, HarnessRegistry, builtin_harness_profile,
};
use kiteframe::store::{HarnessPluginRegistration, PrepareHarnessInvocation};
use kiteframe::{
    AgentCommandContext, AgentCommandId, AgentKernel, AgentRecoveryReport, AgentSelector,
    BuiltinHarnessPlugin, ClaimAgentCommand, CompleteAgentCommand, CreateSession, EvidenceId,
    GoalContract, GoalPhase, IdempotencyKey, KiteAgentRuntime, KiteFrameState, ModelClient,
    ModelError, ModelRequest, ModelResponse, OperationalStatus, PostgresStore, PrincipalId,
    ResumeUserTask, RuntimeCeilings, RuntimeCommandKey, RuntimeEvent, RuntimeStateSnapshot,
    SessionView, StartAgent, StoreError, SubmitUserTask, SuccessCriterion, TaskOwnerAuthorizer,
    TaskPreferences, TaskTarget, ToolCatalogSnapshot, UserTaskId,
};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

static RESTART_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct NeverModel;

#[async_trait::async_trait]
impl ModelClient for NeverModel {
    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        panic!("restart recovery must not invoke the model")
    }
}

#[derive(Clone)]
struct IsolatedDatabase {
    name: String,
    admin_url: String,
    url: String,
}

impl IsolatedDatabase {
    async fn create(base_url: &str) -> Self {
        let mut admin = url::Url::parse(base_url).unwrap();
        admin.set_path("/postgres");
        let name = format!("kiteframe_slice2_task5_restart_{}", Uuid::now_v7().simple());
        let admin_url = admin.to_string();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .unwrap();
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&pool)
            .await
            .unwrap();
        drop(pool);
        admin.set_path(&format!("/{name}"));
        Self {
            name,
            admin_url,
            url: admin.to_string(),
        }
    }

    async fn drop(self) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .unwrap();
        sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
            .execute(&pool)
            .await
            .unwrap();
    }
}

struct RestartAgentFixture {
    kernel: Arc<AgentKernel>,
    store: Arc<PostgresStore>,
    database_url: String,
    database: IsolatedDatabase,
    session: SessionView,
    principal: PrincipalId,
    task_id: UserTaskId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RestartAuthoritySnapshot {
    execution_json: Value,
    command_rows: Vec<Value>,
    transition_count: i64,
    event_count: i64,
}

impl RestartAgentFixture {
    async fn started_fork() -> Option<Self> {
        let base_url = std::env::var("DATABASE_URL").ok()?;
        let database = IsolatedDatabase::create(&base_url).await;
        let database_url = database.url.clone();
        let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let principal = PrincipalId::new(format!("agent-restart-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/agent-restart".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
        store.append_catalog_epoch(&catalog).await.unwrap();
        let name = format!("restart_agent_{}", Uuid::now_v7().simple());
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
    on no_path() {{
      block goal when 0 {{
        code: "no_safe_path"
        explanation: "No safe path remains."
        evidence: [current_evidence]
        unlock: ["A policy revision permits retry."]
      }}
    }}
  }}
  task branch(value: string) -> string {{
    model reasoning prompt """Return.""" tools []
    on ok(result: string) {{ done result }}
  }}
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
        let kernel = Arc::new(AgentKernel::new(
            Arc::clone(&store),
            Arc::new(KiteAgentRuntime::new(RuntimeCeilings::default())),
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
            store,
            database_url,
            database,
            session,
            principal,
            task_id: accepted.id,
        })
    }

    async fn complete_root_into_two_pending_children(&mut self) {
        let pending = self
            .store
            .dispatchable_agent_commands(64)
            .await
            .unwrap()
            .into_iter()
            .find(|command| command.task_id == self.task_id)
            .unwrap();
        let root = self
            .store
            .claim_agent_command(ClaimAgentCommand {
                session_id: self.session.id.clone(),
                task_id: self.task_id.clone(),
                command_id: pending.id,
                expected_command_revision: pending.revision,
                claimant: "builtin-direct-v1".to_owned(),
                idempotency_key: IdempotencyKey::new("claim-restart-root").unwrap(),
            })
            .await
            .unwrap();
        self.kernel
            .complete_command(
                &self.principal,
                CompleteAgentCommand {
                    session_id: self.session.id.clone(),
                    task_id: self.task_id.clone(),
                    command_id: root.id.clone(),
                    expected_command_revision: root.revision,
                    idempotency_key: IdempotencyKey::new("complete-restart-root").unwrap(),
                    event: RuntimeEvent::HarnessResult {
                        event_id: "evt-restart-root".to_owned(),
                        command_key: root.key,
                        outcome: "split".to_owned(),
                        fields: json!({"left":"one", "right":"two"}),
                        evidence_id: EvidenceId::new(Uuid::now_v7()),
                        history: vec![],
                    },
                },
            )
            .await
            .unwrap();
    }

    async fn authority_snapshot(&self) -> RestartAuthoritySnapshot {
        Self::authority_snapshot_from(&self.store, &self.task_id).await
    }

    async fn authority_snapshot_from(
        store: &PostgresStore,
        task_id: &UserTaskId,
    ) -> RestartAuthoritySnapshot {
        let execution_json: Value = sqlx::query_scalar(
            "SELECT jsonb_build_object( \
               'taskState', t.state, 'status', t.operational_status, 'taskRevision', t.revision, \
               'goalPhase', g.phase, 'goalRevision', g.revision, \
               'snapshotRevision', rs.task_revision, 'snapshot', rs.payload, \
               'snapshotDigest', rs.snapshot_digest) \
             FROM user_tasks t JOIN goal_states g ON g.task_id = t.id \
             JOIN agent_runtime_snapshots rs ON rs.task_id = t.id WHERE t.id = $1",
        )
        .bind(task_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap();
        let command_rows: Vec<Value> = sqlx::query_scalar(
            "SELECT jsonb_build_object( \
               'id', id, 'key', runtime_key, 'kind', command_kind, 'payload', payload, \
               'digest', payload_digest, 'state', state, 'revision', revision, \
               'claimant', claimant) FROM agent_commands WHERE task_id = $1 ORDER BY id",
        )
        .bind(task_id.as_uuid())
        .fetch_all(store.pool())
        .await
        .unwrap();
        let transition_count =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_transitions WHERE task_id = $1")
                .bind(task_id.as_uuid())
                .fetch_one(store.pool())
                .await
                .unwrap();
        let event_count =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_events WHERE task_id = $1")
                .bind(task_id.as_uuid())
                .fetch_one(store.pool())
                .await
                .unwrap();
        RestartAuthoritySnapshot {
            execution_json,
            command_rows,
            transition_count,
            event_count,
        }
    }

    async fn first_pending_context(&self) -> AgentCommandContext {
        let command = self
            .store
            .dispatchable_agent_commands(64)
            .await
            .unwrap()
            .into_iter()
            .find(|command| command.task_id == self.task_id && command.revision == 0)
            .unwrap();
        self.store
            .load_agent_command_context(self.session.id.clone(), self.task_id.clone(), command.id)
            .await
            .unwrap()
    }

    async fn corrupt_matching_wait_revision(&self, revision: &str) {
        let context = self.first_pending_context().await;
        let snapshot = context.execution.runtime.unwrap();
        let runtime = KiteAgentRuntime::new(RuntimeCeilings::default());
        let mut state = runtime.decode_snapshot(&snapshot).unwrap();
        let frame = state
            .frames
            .values_mut()
            .find(|frame| {
                matches!(
                    &frame.state,
                    KiteFrameState::WaitingForHarness { command_key }
                        if command_key == &context.command.key
                )
            })
            .unwrap();
        let invalid_key =
            RuntimeCommandKey::new(format!("r{revision}-f{}-harness", frame.id.get())).unwrap();
        frame.state = KiteFrameState::WaitingForHarness {
            command_key: invalid_key.clone(),
        };
        let corrupted = RuntimeStateSnapshot::new(
            snapshot.disposition,
            serde_json::to_value(state).unwrap(),
            &RuntimeCeilings::default(),
        )
        .unwrap();
        sqlx::query("UPDATE agent_commands SET runtime_key = $1 WHERE id = $2")
            .bind(invalid_key.as_str())
            .bind(context.command.id.as_uuid())
            .execute(self.store.pool())
            .await
            .unwrap();
        sqlx::query(
            "UPDATE agent_runtime_snapshots SET payload = $1, canonical_bytes = $2, \
             snapshot_digest = $3 WHERE task_id = $4",
        )
        .bind(&corrupted.payload)
        .bind(i32::try_from(corrupted.canonical_byte_count).unwrap())
        .bind(&corrupted.snapshot_digest)
        .bind(self.task_id.as_uuid())
        .execute(self.store.pool())
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn restart_restores_snapshot_and_pending_commands_without_replay() {
    let _guard = RESTART_TEST_LOCK.lock().await;
    let Some(mut fixture) = RestartAgentFixture::started_fork().await else {
        return;
    };
    fixture.complete_root_into_two_pending_children().await;
    let context = fixture.first_pending_context().await;
    let frozen_hash = context.definition.definition_hash.clone();
    let before = fixture.authority_snapshot().await;
    let database_url = fixture.database_url.clone();
    let task_id = fixture.task_id.clone();
    let session_id = fixture.session.id.clone();
    let database = fixture.database.clone();
    drop(fixture.kernel);
    drop(fixture.store);

    let reopened = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let report = reopened.recover_agent_commands(32).await.unwrap();
    let after = RestartAgentFixture::authority_snapshot_from(&reopened, &task_id).await;
    assert_eq!(
        report,
        AgentRecoveryReport {
            pending: 2,
            claimed: 0
        }
    );
    assert_eq!(after, before);
    assert_eq!(after.transition_count, 2);
    assert_eq!(after.event_count, 3);
    let command_id = AgentCommandId::new(
        Uuid::parse_str(after.command_rows[1]["id"].as_str().unwrap()).unwrap(),
    );
    let recovered = reopened
        .load_agent_command_context(session_id, task_id, command_id)
        .await
        .unwrap();
    assert_eq!(recovered.definition.definition_hash, frozen_hash);
    drop(reopened);
    database.drop().await;
}

#[tokio::test]
async fn restart_preserves_claim_and_excludes_completed_commands() {
    let _guard = RESTART_TEST_LOCK.lock().await;
    let Some(mut fixture) = RestartAgentFixture::started_fork().await else {
        return;
    };
    fixture.complete_root_into_two_pending_children().await;
    let pending = fixture.first_pending_context().await.command;
    fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: pending.id,
            expected_command_revision: pending.revision,
            claimant: "restart-claimant".to_owned(),
            idempotency_key: IdempotencyKey::new("restart-claim").unwrap(),
        })
        .await
        .unwrap();
    let before = fixture.authority_snapshot().await;
    let database = fixture.database.clone();
    let reopened = PostgresStore::connect(&fixture.database_url).await.unwrap();
    assert_eq!(
        reopened.recover_agent_commands(32).await.unwrap(),
        AgentRecoveryReport {
            pending: 1,
            claimed: 1
        }
    );
    assert_eq!(
        RestartAgentFixture::authority_snapshot_from(&reopened, &fixture.task_id).await,
        before
    );
    drop(reopened);
    drop(fixture.kernel);
    drop(fixture.store);
    database.drop().await;
}

#[tokio::test]
async fn restart_recovers_kernel_before_marking_accepted_harness_work_unknown() {
    let _guard = RESTART_TEST_LOCK.lock().await;
    let Some(fixture) = RestartAgentFixture::started_fork().await else {
        eprintln!("SKIP ordered harness restart recovery: DATABASE_URL is absent");
        return;
    };
    let pending = fixture.first_pending_context().await.command;
    let claimed = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: pending.id.clone(),
            expected_command_revision: pending.revision,
            claimant: "harness-dispatcher-v1".to_owned(),
            idempotency_key: IdempotencyKey::new(format!(
                "harness-dispatcher-v1:{}",
                pending.id.as_uuid()
            ))
            .unwrap(),
        })
        .await
        .unwrap();
    let profile = builtin_harness_profile("0123456789abcdef0123456789abcdef01234567").unwrap();
    let profile_digest = profile.digest().unwrap();
    fixture
        .store
        .register_harness_plugin(HarnessPluginRegistration {
            profile_id: profile.id.clone(),
            profile_digest: profile_digest.clone(),
            source_repository: profile.source_repository.clone(),
            source_revision: profile.source_revision.clone(),
            transport: profile.transport,
            enabled: true,
        })
        .await
        .unwrap();
    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    fixture
        .store
        .prepare_harness_invocation(PrepareHarnessInvocation {
            invocation_id: &invocation_id,
            command_id: &claimed.id,
            command_revision: claimed.revision,
            profile_id: &profile.id,
            profile_digest: &profile_digest,
            payload_digest: &claimed.payload_digest,
        })
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation_id, 0)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation_id, 0)
        .await
        .unwrap();

    let database = fixture.database.clone();
    let database_url = fixture.database_url.clone();
    let session_id = fixture.session.id.clone();
    let task_id = fixture.task_id.clone();
    drop(fixture.kernel);
    drop(fixture.store);

    let reopened = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    let kernel = Arc::new(AgentKernel::new(
        Arc::clone(&reopened),
        Arc::new(KiteAgentRuntime::new(RuntimeCeilings::default())),
        Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&reopened))),
    ));
    assert_eq!(
        kernel.recover(32).await.unwrap(),
        AgentRecoveryReport {
            pending: 0,
            claimed: 1,
        }
    );
    let mut registry = HarnessRegistry::new();
    registry
        .register(Arc::new(BuiltinHarnessPlugin::new(
            profile,
            Arc::new(NeverModel),
        )))
        .await
        .unwrap();
    let dispatcher = kernel
        .clone()
        .with_harness_registry(Arc::new(registry), Arc::new(DenyAllHarnessApprovals));
    let report = dispatcher.recover_harness_invocations(32).await.unwrap();
    assert_eq!(report.outcome_unknown, 1);
    assert_eq!(report.retryable_pre_accept, 0);
    assert_eq!(report.terminal, 0);
    let context = reopened
        .load_agent_command_context(session_id, task_id, claimed.id)
        .await
        .unwrap();
    assert_eq!(context.command.state, kiteframe::AgentCommandState::Failed);
    assert_eq!(context.execution.goal_phase, GoalPhase::Active);
    drop(dispatcher);
    drop(kernel);
    drop(reopened);
    database.drop().await;
}

#[tokio::test]
async fn restart_excludes_blocked_work_until_authenticated_resume_reproposes_it() {
    let _guard = RESTART_TEST_LOCK.lock().await;
    let Some(fixture) = RestartAgentFixture::started_fork().await else {
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
    let source = fixture
        .store
        .claim_agent_command(ClaimAgentCommand {
            session_id: fixture.session.id.clone(),
            task_id: fixture.task_id.clone(),
            command_id: pending.id,
            expected_command_revision: pending.revision,
            claimant: "builtin-direct-v1".to_owned(),
            idempotency_key: IdempotencyKey::new("claim-block-before-restart").unwrap(),
        })
        .await
        .unwrap();
    let blocked = fixture
        .kernel
        .complete_command(
            &fixture.principal,
            CompleteAgentCommand {
                session_id: fixture.session.id.clone(),
                task_id: fixture.task_id.clone(),
                command_id: source.id,
                expected_command_revision: source.revision,
                idempotency_key: IdempotencyKey::new("block-before-restart").unwrap(),
                event: RuntimeEvent::HarnessResult {
                    event_id: "evt-block-before-restart".to_owned(),
                    command_key: source.key,
                    outcome: "no_path".to_owned(),
                    fields: json!({}),
                    evidence_id: EvidenceId::new(Uuid::now_v7()),
                    history: vec![],
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(blocked.goal_phase, GoalPhase::Blocked);
    assert_eq!(blocked.operational_status, OperationalStatus::Blocked);
    let database = fixture.database.clone();
    let database_url = fixture.database_url.clone();
    let session_id = fixture.session.id.clone();
    let task_id = fixture.task_id.clone();
    let principal = fixture.principal.clone();
    drop(fixture.kernel);
    drop(fixture.store);

    let reopened = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
    assert_eq!(
        reopened.recover_agent_commands(32).await.unwrap(),
        AgentRecoveryReport {
            pending: 0,
            claimed: 0,
        }
    );
    let kernel = AgentKernel::new(
        Arc::clone(&reopened),
        Arc::new(KiteAgentRuntime::new(RuntimeCeilings::default())),
        Arc::new(TaskOwnerAuthorizer::new(Arc::clone(&reopened))),
    );
    let resumed = kernel
        .resume_task(
            &principal,
            ResumeUserTask {
                session_id,
                task_id,
                expected_task_revision: blocked.task_revision,
                expected_goal_revision: blocked.goal_revision,
                idempotency_key: IdempotencyKey::new("resume-after-restart").unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(resumed.goal_phase, GoalPhase::Active);
    assert_eq!(
        resumed.operational_status,
        OperationalStatus::WaitingForHarness
    );
    assert_eq!(
        reopened.recover_agent_commands(32).await.unwrap(),
        AgentRecoveryReport {
            pending: 1,
            claimed: 0,
        }
    );
    drop(kernel);
    drop(reopened);
    database.drop().await;
}

#[tokio::test]
async fn recovery_rejects_corrupt_snapshot_command_and_wait_mismatch_without_repair() {
    let _guard = RESTART_TEST_LOCK.lock().await;
    for corruption in [
        "snapshot",
        "command_digest",
        "completed_command_digest",
        "wait_mismatch",
        "wait_revision_zero",
        "wait_revision_future",
        "wait_revision_noncanonical",
    ] {
        let Some(mut fixture) = RestartAgentFixture::started_fork().await else {
            return;
        };
        fixture.complete_root_into_two_pending_children().await;
        let database = fixture.database.clone();
        match corruption {
            "snapshot" => {
                sqlx::query(
                    "UPDATE agent_runtime_snapshots SET snapshot_digest = 'sha256:corrupt' \
                     WHERE task_id = $1",
                )
                .bind(fixture.task_id.as_uuid())
                .execute(fixture.store.pool())
                .await
                .unwrap();
            }
            "command_digest" => {
                sqlx::query(
                    "UPDATE agent_commands SET payload_digest = 'sha256:corrupt' \
                     WHERE task_id = $1 AND state = 'pending'",
                )
                .bind(fixture.task_id.as_uuid())
                .execute(fixture.store.pool())
                .await
                .unwrap();
            }
            "completed_command_digest" => {
                sqlx::query(
                    "UPDATE agent_commands SET payload_digest = 'sha256:corrupt' \
                     WHERE task_id = $1 AND state = 'completed'",
                )
                .bind(fixture.task_id.as_uuid())
                .execute(fixture.store.pool())
                .await
                .unwrap();
            }
            "wait_mismatch" => {
                sqlx::query(
                    "UPDATE agent_commands SET runtime_key = runtime_key || '-mismatch' \
                     WHERE id = (SELECT id FROM agent_commands WHERE task_id = $1 \
                                  AND state = 'pending' ORDER BY id LIMIT 1)",
                )
                .bind(fixture.task_id.as_uuid())
                .execute(fixture.store.pool())
                .await
                .unwrap();
            }
            "wait_revision_zero" => fixture.corrupt_matching_wait_revision("0").await,
            "wait_revision_future" => fixture.corrupt_matching_wait_revision("999").await,
            "wait_revision_noncanonical" => fixture.corrupt_matching_wait_revision("02").await,
            _ => unreachable!(),
        }
        let corrupted = fixture.authority_snapshot().await;
        assert!(matches!(
            fixture.store.recover_agent_commands(32).await,
            Err(StoreError::RuntimeStateInvalid(_)) | Err(StoreError::AgentTransitionInvalid(_))
        ));
        assert_eq!(
            fixture.authority_snapshot().await,
            corrupted,
            "{corruption}"
        );
        drop(fixture.kernel);
        drop(fixture.store);
        database.drop().await;
    }
}
