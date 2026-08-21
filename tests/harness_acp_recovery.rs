use kiteframe::HarnessRecoveryReport;

#[cfg(feature = "test-support")]
static RECOVERY_DATABASE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[test]
fn harness_recovery_report_counts_each_durable_classification() {
    let report = HarnessRecoveryReport {
        retryable_pre_accept: 2,
        outcome_unknown: 3,
        terminal: 5,
    };

    assert_eq!(report.retryable_pre_accept, 2);
    assert_eq!(report.outcome_unknown, 3);
    assert_eq!(report.terminal, 5);
}

#[cfg(feature = "test-support")]
mod postgres_recovery_boundaries {
    use std::sync::Arc;

    use kiteframe::harness::{HarnessInvocationId, HarnessProfileId, HarnessTransport};
    use kiteframe::store::{
        HarnessInvocationState, HarnessPluginRegistration, PrepareHarnessInvocation,
    };
    use kiteframe::{AgentCommandId, CreateSession, PostgresStore, PrincipalId};
    use serde_json::json;
    use sqlx::PgPool;
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    struct DbFixture {
        store: Arc<PostgresStore>,
        pool: PgPool,
        task_id: Uuid,
        transition_id: Uuid,
        profile_id: HarnessProfileId,
        profile_digest: String,
        next_ordinal: i32,
    }

    impl DbFixture {
        async fn new() -> Option<Self> {
            let Some(database_url) = std::env::var("DATABASE_URL").ok() else {
                eprintln!(
                    "SKIP harness_acp_recovery postgres crash matrix: DATABASE_URL is not set"
                );
                return None;
            };
            let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
            let pool = store.pool().clone();
            let principal = PrincipalId::new(format!("acp-recovery-{}", Uuid::now_v7())).unwrap();
            let session = store
                .create_session(CreateSession {
                    principal,
                    working_directory: "/acp-recovery".to_owned(),
                    retention_until: OffsetDateTime::now_utc() + Duration::days(1),
                })
                .await
                .unwrap();
            let definition_id = Uuid::now_v7();
            let task_id = Uuid::now_v7();
            let transition_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO workflow_definitions \
                 (id, name, version, definition_hash, source, compiled_definition, tool_bindings, definition_kind) \
                 VALUES ($1, $2, 1, $3, 'agent example {}', '{}'::jsonb, '[]'::jsonb, 'agent')",
            )
            .bind(definition_id)
            .bind(format!("acp-recovery-{}", Uuid::now_v7()))
            .bind(format!("sha256:{}", Uuid::now_v7()))
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO user_tasks \
                 (id, session_id, principal, definition_id, submission_idempotency_key, request_digest, target, preferences) \
                 VALUES ($1, $2, 'acp-recovery', $3, $4, 'sha256:task', '{}'::jsonb, '{}'::jsonb)",
            )
            .bind(task_id)
            .bind(session.id.as_uuid())
            .bind(definition_id)
            .bind(format!("submit-{}", Uuid::now_v7()))
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO goal_states (task_id) VALUES ($1)")
                .bind(task_id)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO agent_transitions \
                 (id, task_id, idempotency_key, request_digest, expected_task_revision, expected_goal_revision, \
                  committed_task_revision, committed_goal_revision, snapshot_digest, command_digests) \
                 VALUES ($1, $2, $3, 'sha256:transition', 0, 0, 1, 0, 'sha256:snapshot', '[]'::jsonb)",
            )
            .bind(transition_id)
            .bind(task_id)
            .bind(format!("transition-{}", Uuid::now_v7()))
            .execute(&pool)
            .await
            .unwrap();
            let profile_id =
                HarnessProfileId::new(format!("acp-recovery-{}", Uuid::now_v7())).unwrap();
            let profile_digest = "sha256:acp-recovery-profile".to_owned();
            store
                .register_harness_plugin(HarnessPluginRegistration {
                    profile_id: profile_id.clone(),
                    profile_digest: profile_digest.clone(),
                    source_repository: "https://example.test/acp-recovery".to_owned(),
                    source_revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    transport: HarnessTransport::AcpStdio,
                    enabled: true,
                })
                .await
                .unwrap();
            Some(Self {
                store,
                pool,
                task_id,
                transition_id,
                profile_id,
                profile_digest,
                next_ordinal: 0,
            })
        }

        async fn prepare(&mut self, suffix: &str) -> kiteframe::store::HarnessInvocationView {
            let command_id = AgentCommandId::new(Uuid::now_v7());
            let runtime_key = format!("harness:recovery:{suffix}");
            let payload_digest = format!("sha256:{suffix}");
            sqlx::query(
                "INSERT INTO agent_commands \
                 (id, task_id, transition_id, ordinal, schema_version, runtime_key, command_kind, payload, payload_digest, state, claimant) \
                 VALUES ($1, $2, $3, $4, 1, $5, 'invoke_harness', $6, $7, 'claimed', 'harness-dispatcher-v1')",
            )
            .bind(command_id.as_uuid())
            .bind(self.task_id)
            .bind(self.transition_id)
            .bind(self.next_ordinal)
            .bind(runtime_key)
            .bind(json!({"type": "invoke_harness", "payload": {}}))
            .bind(&payload_digest)
            .execute(&self.pool)
            .await
            .unwrap();
            self.next_ordinal += 1;
            self.store
                .prepare_harness_invocation(PrepareHarnessInvocation {
                    invocation_id: &HarnessInvocationId::new(Uuid::now_v7()),
                    command_id: &command_id,
                    command_revision: 0,
                    profile_id: &self.profile_id,
                    profile_digest: &self.profile_digest,
                    payload_digest: &payload_digest,
                })
                .await
                .unwrap()
        }

        async fn authority_snapshot(&self) -> (String, i64, i64) {
            let goal: (String, i64) =
                sqlx::query_as("SELECT phase, revision FROM goal_states WHERE task_id = $1")
                    .bind(self.task_id)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap();
            let evidence: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM goal_evidence WHERE task_id = $1")
                    .bind(self.task_id)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap();
            (goal.0, goal.1, evidence)
        }
    }

    #[tokio::test]
    async fn postgres_restart_matrix_preserves_identity_bound_and_goal_authority() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut fixture) = DbFixture::new().await else {
            return;
        };
        let authority_before = fixture.authority_snapshot().await;

        let retryable = fixture.prepare("retryable").await;
        fixture
            .store
            .mark_harness_invocation_started(&retryable.invocation_id, 0)
            .await
            .unwrap();
        let retried = fixture
            .store
            .record_harness_retryable_failure(
                &retryable.invocation_id,
                0,
                3,
                "sha256:retryable-failure",
                &json!({"type":"failed","code":"process_lost","detail":"before acceptance"}),
            )
            .await
            .unwrap();
        assert_eq!(retried.invocation_id, retryable.invocation_id);
        assert_eq!(retried.profile_id, retryable.profile_id);
        assert_eq!(
            retried.attempt, 1,
            "one failed attempt increments exactly once"
        );

        let accepted = fixture.prepare("accepted").await;
        fixture
            .store
            .mark_harness_invocation_started(&accepted.invocation_id, 0)
            .await
            .unwrap();
        fixture
            .store
            .mark_harness_prompt_accepted(&accepted.invocation_id, 0)
            .await
            .unwrap();

        let committed = fixture.prepare("committed").await;
        let committed_terminal_key =
            format!("harness-terminal:{}", committed.invocation_id.as_uuid());
        fixture
            .store
            .mark_harness_invocation_started(&committed.invocation_id, 0)
            .await
            .unwrap();
        fixture
            .store
            .mark_harness_prompt_accepted(&committed.invocation_id, 0)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE agent_commands SET state = 'completed', revision = revision + 1, \
             completion_idempotency_key = $1, completion_request_digest = 'sha256:completed', \
             result = $2 WHERE id = $3",
        )
        .bind(format!(
            "harness-result:{}",
            committed.invocation_id.as_uuid()
        ))
        .bind(json!({
            "type":"harness_result",
            "payload":{
                "event_id":format!("harness-result:{}", committed.invocation_id.as_uuid()),
                "command_key":"harness:recovery:committed",
                "outcome":"ok",
                "fields":{"answer":"once"},
                "evidence_id":committed.invocation_id.as_uuid(),
                "history":[]
            }
        }))
        .bind(committed.command_id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap();

        let mut excluded: Vec<HarnessInvocationId> = sqlx::query_scalar(
            "SELECT invocations.invocation_id FROM harness_invocations AS invocations \
             JOIN agent_commands AS commands ON commands.id = invocations.command_id \
             WHERE commands.task_id <> $1",
        )
        .bind(fixture.task_id)
        .fetch_all(&fixture.pool)
        .await
        .unwrap()
        .into_iter()
        .map(HarnessInvocationId::new)
        .collect();
        let mut recovered = Vec::new();
        loop {
            let page = fixture
                .store
                .recover_harness_invocations_excluding(1, &excluded)
                .await
                .unwrap();
            assert!(page.len() <= 1, "limit=1 is a total classification bound");
            let Some(row) = page.into_iter().next() else {
                break;
            };
            excluded.push(row.invocation_id.clone());
            recovered.push(row);
        }
        assert_eq!(
            recovered.len(),
            3,
            "bounded calls drain every recovery class"
        );
        let unknown = recovered
            .iter()
            .find(|row| row.invocation_id == accepted.invocation_id)
            .unwrap();
        assert_eq!(unknown.state, HarnessInvocationState::Accepted);
        assert_eq!(unknown.attempt, 0);
        let retry = recovered
            .iter()
            .find(|row| row.invocation_id == retryable.invocation_id)
            .unwrap();
        assert_eq!(retry.state, HarnessInvocationState::Started);
        assert_eq!(retry.attempt, 1, "recovery does not increment retry count");
        let terminal = recovered
            .iter()
            .find(|row| row.invocation_id == committed.invocation_id)
            .unwrap();
        assert_eq!(terminal.state, HarnessInvocationState::Completed);
        assert_eq!(
            terminal.terminal_request_digest.as_deref(),
            Some(committed_terminal_key.as_str())
        );
        assert_eq!(fixture.authority_snapshot().await, authority_before);

        sqlx::query(
            "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 \
             WHERE task_id = $1 AND state = 'claimed'",
        )
        .bind(fixture.task_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
        fixture
            .store
            .refresh_harness_recovery_invocation(&retryable.invocation_id)
            .await
            .unwrap();
        fixture
            .store
            .refresh_harness_recovery_invocation(&accepted.invocation_id)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE agent_commands SET completion_idempotency_key = $1, \
             completion_request_digest = 'sha256:cleanup', failure = '{\"code\":\"cleanup\"}'::jsonb \
             WHERE task_id = $2 AND state = 'cancelled' AND completion_idempotency_key IS NULL",
        )
        .bind(format!("cleanup-{}", Uuid::now_v7()))
        .bind(fixture.task_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
}

#[cfg(feature = "test-support")]
mod fake_acp_boundaries {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use kiteframe::harness::{
        AcpCommand, AcpHarnessPlugin, AcpLimits, AcpTestBarrier, AcpTestBarrierPoint,
        HarnessAcceptanceBarrier, HarnessApprovalBroker, HarnessApprovalDecision,
        HarnessApprovalRequest, HarnessCapability, HarnessEventEnvelope, HarnessEventSink,
        HarnessFailure, HarnessInvocationId, HarnessInvocationRequest, HarnessProfile,
        HarnessProfileId, HarnessRegistry, HarnessTransport,
    };
    use kiteframe::{
        AgentCommandId, ClosedSchema, GoalProjection, HarnessStructuralLimits, InvokeHarness,
        ModelRole, PrimitiveValue,
    };
    use serde_json::{Value, json};
    use tokio::sync::Notify;
    use uuid::Uuid;

    pub(super) const SOURCE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<HarnessEventEnvelope>>);

    #[async_trait]
    impl HarnessEventSink for RecordingSink {
        async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    struct AllowOnce;

    #[async_trait]
    impl HarnessApprovalBroker for AllowOnce {
        async fn decide(&self, _request: HarnessApprovalRequest) -> HarnessApprovalDecision {
            HarnessApprovalDecision::AllowOnce
        }
    }

    struct AcceptanceGate {
        entered: Notify,
        release: Notify,
    }

    impl AcceptanceGate {
        fn new() -> Self {
            Self {
                entered: Notify::new(),
                release: Notify::new(),
            }
        }
    }

    #[async_trait]
    impl HarnessAcceptanceBarrier for AcceptanceGate {
        async fn commit(&self) -> Result<(), HarnessFailure> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    struct ImmediateAcceptance;

    #[async_trait]
    impl HarnessAcceptanceBarrier for ImmediateAcceptance {
        async fn commit(&self) -> Result<(), HarnessFailure> {
            Ok(())
        }
    }

    struct FakeRun {
        workspace: PathBuf,
        observation: PathBuf,
        registry: Arc<HarnessRegistry>,
        request: HarnessInvocationRequest,
        sink: Arc<RecordingSink>,
    }

    impl FakeRun {
        async fn new(barrier: Arc<AcpTestBarrier>) -> Self {
            let workspace =
                std::env::temp_dir().join(format!("kiteframe-recovery-{}", Uuid::now_v7()));
            std::fs::create_dir_all(&workspace).unwrap();
            let observation = workspace.join("observation.json");
            let profile = profile();
            let plugin = AcpHarnessPlugin::with_environment(
                profile.clone(),
                AcpCommand {
                    program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
                    args: vec![],
                    source_revision: SOURCE_REVISION.to_owned(),
                },
                limits(),
                BTreeMap::from([
                    (
                        OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                        fixture("core-success.json").as_os_str().to_owned(),
                    ),
                    (
                        OsString::from("KITEFRAME_FAKE_ACP_OBSERVATION"),
                        observation.as_os_str().to_owned(),
                    ),
                    (
                        OsString::from("KITEFRAME_FAKE_ACP_ALLOW_REGISTRATION_EOF"),
                        OsString::from("1"),
                    ),
                ]),
            )
            .with_test_barrier(barrier);
            let mut registry = HarnessRegistry::new();
            registry.register(Arc::new(plugin)).await.unwrap();
            let request = request(profile.id, workspace.clone());
            Self {
                workspace,
                observation,
                registry: Arc::new(registry),
                request,
                sink: Arc::new(RecordingSink::default()),
            }
        }

        fn observation(&self) -> Value {
            std::fs::read(&self.observation)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_else(|| json!({"methods": []}))
        }

        async fn wait_for_method_count(&self, count: usize) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            loop {
                if self.observation()["methods"]
                    .as_array()
                    .is_some_and(|methods| methods.len() >= count)
                {
                    return;
                }
                assert!(tokio::time::Instant::now() < deadline);
                tokio::task::yield_now().await;
            }
        }
    }

    impl Drop for FakeRun {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }

    #[tokio::test]
    async fn fake_acp_crash_barriers_prove_only_post_write_can_observe_a_prompt() {
        for (method, point, prior_methods, prompt_count) in [
            ("initialize", AcpTestBarrierPoint::BeforeWrite, 0, 0),
            ("session/new", AcpTestBarrierPoint::BeforeWrite, 1, 0),
            ("session/prompt", AcpTestBarrierPoint::BeforeWrite, 2, 0),
            (
                "session/prompt",
                AcpTestBarrierPoint::BeforeAcknowledgement,
                3,
                1,
            ),
        ] {
            let barrier = Arc::new(AcpTestBarrier::new_disarmed(method, point));
            let run = Arc::new(FakeRun::new(barrier.clone()).await);
            barrier.arm();
            let task = tokio::spawn({
                let run = run.clone();
                async move {
                    run.registry
                        .invoke_with_acceptance_barrier(
                            run.request.clone(),
                            Arc::new(AllowOnce),
                            run.sink.clone(),
                            Arc::new(ImmediateAcceptance),
                        )
                        .await
                }
            });
            barrier.wait_until_entered().await;
            run.wait_for_method_count(prior_methods).await;
            let observed = run.observation();
            assert_eq!(
                observed["methods"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|method| *method == "session/prompt")
                    .count(),
                prompt_count,
                "{method:?} {point:?}"
            );
            task.abort();
            barrier.release();
            assert!(task.await.unwrap_err().is_cancelled());
        }
    }

    #[tokio::test]
    async fn crash_before_acceptance_never_writes_prompt_and_post_result_boundary_writes_once() {
        let wire_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let run = Arc::new(FakeRun::new(wire_barrier.clone()).await);
        wire_barrier.arm();
        let acceptance = Arc::new(AcceptanceGate::new());
        let task = tokio::spawn({
            let run = run.clone();
            let acceptance = acceptance.clone();
            async move {
                run.registry
                    .invoke_with_acceptance_barrier(
                        run.request.clone(),
                        Arc::new(AllowOnce),
                        run.sink.clone(),
                        acceptance,
                    )
                    .await
            }
        });
        acceptance.entered.notified().await;
        run.wait_for_method_count(2).await;
        assert!(
            !run.observation()["methods"]
                .as_array()
                .unwrap()
                .iter()
                .any(|method| method == "session/prompt")
        );
        task.abort();
        acceptance.release.notify_waiters();
        assert!(task.await.unwrap_err().is_cancelled());

        let shutdown_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "adapter/shutdown",
            AcpTestBarrierPoint::BeforeAdapterShutdown,
        ));
        let run = Arc::new(FakeRun::new(shutdown_barrier.clone()).await);
        shutdown_barrier.arm();
        let task = tokio::spawn({
            let run = run.clone();
            async move {
                run.registry
                    .invoke_with_acceptance_barrier(
                        run.request.clone(),
                        Arc::new(AllowOnce),
                        run.sink.clone(),
                        Arc::new(ImmediateAcceptance),
                    )
                    .await
            }
        });
        shutdown_barrier.wait_until_entered().await;
        let observed = run.observation();
        assert_eq!(
            observed["methods"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|method| *method == "session/prompt")
                .count(),
            1
        );
        assert!(run.sink.0.lock().unwrap().iter().any(|event| matches!(
            event.event,
            kiteframe::harness::HarnessInvocationEvent::AssistantMessage { .. }
        )));
        task.abort();
        shutdown_barrier.release();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    pub(super) fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp")
            .join(name)
    }

    pub(super) fn profile() -> HarnessProfile {
        HarnessProfile {
            id: HarnessProfileId::new("recovery-fake-acp-v1").unwrap(),
            transport: HarnessTransport::AcpStdio,
            source_repository: "https://example.test/recovery-fake.git".to_owned(),
            source_revision: SOURCE_REVISION.to_owned(),
            program: "kiteframe-fake-acp".to_owned(),
            args: vec![],
            required: BTreeSet::from([
                HarnessCapability::FreshSession,
                HarnessCapability::TextPrompt,
                HarnessCapability::PermissionOnce,
                HarnessCapability::Cancel,
                HarnessCapability::CommittedOutput,
            ]),
            optional: BTreeSet::new(),
            fresh_sessions_only: true,
        }
    }

    pub(super) fn limits() -> AcpLimits {
        AcpLimits {
            max_frame_bytes: 64 * 1024,
            shutdown_grace: Duration::from_millis(250),
            max_pre_accept_attempts: 1,
        }
    }

    fn request(profile_id: HarnessProfileId, workspace: PathBuf) -> HarnessInvocationRequest {
        HarnessInvocationRequest {
            invocation_id: HarnessInvocationId::new(Uuid::now_v7()),
            command_id: AgentCommandId::new(Uuid::now_v7()),
            command_revision: 0,
            profile_id,
            workspace,
            payload_digest: "sha256:recovery".to_owned(),
            payload: InvokeHarness {
                role: ModelRole::Reasoning,
                prompt: "finish".to_owned(),
                goal: GoalProjection {
                    objective: "test recovery".to_owned(),
                    success: vec![],
                    constraints: vec![],
                    blocked_when: vec![],
                },
                input: json!({}),
                input_schema: ClosedSchema::object("input", BTreeMap::new()),
                result_schema: ClosedSchema::object(
                    "result",
                    BTreeMap::from([
                        (
                            "outcome".to_owned(),
                            ClosedSchema::enumeration(BTreeSet::from([PrimitiveValue::String(
                                "ok".to_owned(),
                            )])),
                        ),
                        (
                            "fields".to_owned(),
                            ClosedSchema::object(
                                "outcomes",
                                BTreeMap::from([(
                                    "ok".to_owned(),
                                    ClosedSchema::object(
                                        "ok_fields",
                                        BTreeMap::from([(
                                            "answer".to_owned(),
                                            ClosedSchema::string(),
                                        )]),
                                    ),
                                )]),
                            ),
                        ),
                    ]),
                ),
                tools: vec![],
                projected_history: vec![],
                limits: HarnessStructuralLimits {
                    model_turns_remaining: 1,
                    tool_calls_per_response: 1,
                    response_item_bytes: 4096,
                    active_execution_millis_remaining: 5_000,
                },
            },
        }
    }
}

#[cfg(feature = "test-support")]
mod integrated_postgres_fake_restart {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Arc;

    use kiteframe::dsl::{Limits, compile};
    use kiteframe::harness::{
        AcpCommand, AcpHarnessPlugin, AcpTestBarrier, AcpTestBarrierPoint, DenyAllHarnessApprovals,
        HarnessRegistry, HarnessRouteTable,
    };
    use kiteframe::store::{
        HarnessInvocationState, HarnessPluginRegistration, PrepareHarnessInvocation,
    };
    use kiteframe::{
        AgentCommandDispatcher, AgentCommandId, AgentKernel, AgentSelector, ClaimAgentCommand,
        CompleteAgentCommand, CreateSession, EvidenceId, GoalContract, GoalPhase, IdempotencyKey,
        PrincipalId, RuntimeCeilings, RuntimeCommandKey, RuntimeEvent, StartAgent, SubmitUserTask,
        SuccessCriterion, TaskOwnerAuthorizer, TaskPreferences, TaskTarget, ToolCatalogSnapshot,
        UserTaskId,
    };
    use serde_json::{Value, json};
    use sqlx::postgres::PgPoolOptions;
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::fake_acp_boundaries::{SOURCE_REVISION, fixture, limits, profile};

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
            let name = format!("kiteframe_task7_acp_recovery_{}", Uuid::now_v7().simple());
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

    struct RecoveryFixture {
        database: IsolatedDatabase,
        database_url: String,
        store: Arc<kiteframe::PostgresStore>,
        kernel: Arc<AgentKernel>,
        principal: PrincipalId,
        session_id: kiteframe::SessionId,
        task_id: UserTaskId,
        workspace: PathBuf,
        observation_index: usize,
    }

    impl RecoveryFixture {
        async fn new() -> Option<Self> {
            let Some(base_url) = std::env::var("DATABASE_URL").ok() else {
                eprintln!(
                    "SKIP integrated PostgreSQL/fake-ACP restart fixture: DATABASE_URL is not set"
                );
                return None;
            };
            let database = IsolatedDatabase::create(&base_url).await;
            let database_url = database.url.clone();
            let store = Arc::new(
                kiteframe::PostgresStore::connect(&database_url)
                    .await
                    .unwrap(),
            );
            let principal =
                PrincipalId::new(format!("integrated-acp-recovery-{}", Uuid::now_v7())).unwrap();
            let workspace = std::env::temp_dir().join(format!(
                "kiteframe-integrated-acp-recovery-{}",
                Uuid::now_v7()
            ));
            std::fs::create_dir_all(&workspace).unwrap();
            let session = store
                .create_session(CreateSession {
                    principal: principal.clone(),
                    working_directory: workspace.to_string_lossy().into_owned(),
                    retention_until: OffsetDateTime::now_utc() + Duration::days(1),
                })
                .await
                .unwrap();
            let catalog =
                ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), vec![]).unwrap();
            store.append_catalog_epoch(&catalog).await.unwrap();
            let name = format!("integrated_recovery_{}", Uuid::now_v7().simple());
            let source = format!(
                r#"agent {name}(repository: string, pull_request: integer) -> string {{
  entry inspect(repository = repository, pull_request = pull_request)
  limits {{ steps: 20 model_turns_per_task: 3 tool_calls_per_response: 1 parallel_branches: 1 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task inspect(repository: string, pull_request: integer) -> string {{
    model reasoning prompt """Inspect and finish one bounded task.""" tools []
    on ok(answer: string) {{ done answer }}
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
                            objective: "Prove integrated recovery.".to_owned(),
                            success: vec![SuccessCriterion {
                                id: "result_stable".to_owned(),
                                statement: "One stable result exists.".to_owned(),
                                evidence: "harness.invocation_result".to_owned(),
                            }],
                            constraints: vec![],
                            blocked_when: vec![],
                        },
                        idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                            .unwrap(),
                    },
                )
                .await
                .unwrap();
            let kernel = Arc::new(AgentKernel::new(
                store.clone(),
                Arc::new(kiteframe::KiteAgentRuntime::new(RuntimeCeilings::default())),
                Arc::new(TaskOwnerAuthorizer::new(store.clone())),
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
                database,
                database_url,
                store,
                kernel,
                principal,
                session_id: session.id,
                task_id: accepted.id,
                workspace,
                observation_index: 0,
            })
        }

        async fn cleanup(self) {
            let Self {
                database,
                store,
                kernel,
                workspace,
                ..
            } = self;
            drop(kernel);
            drop(store);
            let _ = std::fs::remove_dir_all(workspace);
            database.drop().await;
        }

        async fn dispatcher(
            &mut self,
            script: &str,
            barrier: Arc<AcpTestBarrier>,
        ) -> (Arc<AgentCommandDispatcher>, PathBuf) {
            self.observation_index += 1;
            let observation = self
                .workspace
                .join(format!("observation-{}.json", self.observation_index));
            let store = Arc::new(
                kiteframe::PostgresStore::connect(&self.database_url)
                    .await
                    .unwrap(),
            );
            let kernel = Arc::new(AgentKernel::new(
                store.clone(),
                Arc::new(kiteframe::KiteAgentRuntime::new(RuntimeCeilings::default())),
                Arc::new(TaskOwnerAuthorizer::new(store.clone())),
            ));
            let harness_profile = profile();
            let plugin = AcpHarnessPlugin::with_environment(
                harness_profile.clone(),
                AcpCommand {
                    program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
                    args: vec![],
                    source_revision: SOURCE_REVISION.to_owned(),
                },
                limits(),
                BTreeMap::from([
                    (
                        OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                        fixture(script).as_os_str().to_owned(),
                    ),
                    (
                        OsString::from("KITEFRAME_FAKE_ACP_OBSERVATION"),
                        observation.as_os_str().to_owned(),
                    ),
                    (
                        OsString::from("KITEFRAME_FAKE_ACP_ALLOW_REGISTRATION_EOF"),
                        OsString::from("1"),
                    ),
                ]),
            )
            .with_test_barrier(barrier);
            let pre_accept_limit = plugin.pre_accept_attempt_limit();
            let mut registry = HarnessRegistry::new();
            registry
                .register_with_pre_accept_attempt_limit(Arc::new(plugin), pre_accept_limit)
                .await
                .unwrap();
            store
                .register_harness_plugin(HarnessPluginRegistration {
                    profile_id: harness_profile.id.clone(),
                    profile_digest: harness_profile.digest().unwrap(),
                    source_repository: harness_profile.source_repository.clone(),
                    source_revision: harness_profile.source_revision.clone(),
                    transport: harness_profile.transport,
                    enabled: true,
                })
                .await
                .unwrap();
            let routes =
                HarnessRouteTable::new(harness_profile.id.clone(), harness_profile.id.clone());
            (
                Arc::new(kernel.with_harness_registry_routes(
                    Arc::new(registry),
                    Arc::new(DenyAllHarnessApprovals),
                    routes,
                )),
                observation,
            )
        }

        async fn invocation(&self) -> Option<kiteframe::store::HarnessInvocationView> {
            let command = self.pending_or_owned_command().await;
            let revision: Option<i64> = sqlx::query_scalar(
                "SELECT command_revision FROM harness_invocations WHERE command_id = $1",
            )
            .bind(command.id.as_uuid())
            .fetch_optional(self.store.pool())
            .await
            .unwrap();
            let revision = revision?;
            self.store
                .harness_invocation_for_command_revision(
                    &command.id,
                    u64::try_from(revision).unwrap(),
                )
                .await
                .unwrap()
        }

        async fn pending_or_owned_command(&self) -> kiteframe::AgentCommandView {
            let id: Uuid = sqlx::query_scalar(
                "SELECT id FROM agent_commands WHERE task_id = $1 AND command_kind = 'invoke_harness' ORDER BY ordinal LIMIT 1",
            )
            .bind(self.task_id.as_uuid())
            .fetch_one(self.store.pool())
            .await
            .unwrap();
            self.store
                .load_agent_command_context(
                    self.session_id.clone(),
                    self.task_id.clone(),
                    AgentCommandId::new(id),
                )
                .await
                .unwrap()
                .command
        }

        async fn authority(&self) -> (GoalPhase, i64) {
            let execution = self
                .store
                .get_agent_execution(self.session_id.clone(), self.task_id.clone())
                .await
                .unwrap();
            let evidence: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM goal_evidence WHERE task_id = $1")
                    .bind(self.task_id.as_uuid())
                    .fetch_one(self.store.pool())
                    .await
                    .unwrap();
            (execution.goal_phase, evidence)
        }
    }

    fn prompt_count(path: &PathBuf) -> usize {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["methods"].as_array().cloned())
            .map_or(0, |methods| {
                methods
                    .iter()
                    .filter(|method| *method == "session/prompt")
                    .count()
            })
    }

    #[tokio::test]
    async fn restart_before_insert_after_prepare_and_after_retryable_loss_keeps_one_identity() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut before_insert) = RecoveryFixture::new().await else {
            return;
        };
        let barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (first_dispatcher, _) = before_insert.dispatcher("core-success.json", barrier).await;
        assert!(before_insert.invocation().await.is_none());
        drop(first_dispatcher);
        let restart_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (dispatcher, observation) = before_insert
            .dispatcher("core-success.json", restart_barrier)
            .await;
        assert_eq!(
            dispatcher.recover_harness_invocations(1).await.unwrap(),
            kiteframe::HarnessRecoveryReport::default()
        );
        assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 1);
        let inserted = before_insert.invocation().await.unwrap();
        assert_eq!(prompt_count(&observation), 1);
        assert_eq!(inserted.attempt, 0);

        let Some(mut after_prepare) = RecoveryFixture::new().await else {
            return;
        };
        let registration_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (first_dispatcher, _) = after_prepare
            .dispatcher("core-success.json", registration_barrier)
            .await;
        let pending = after_prepare.pending_or_owned_command().await;
        let claimed = after_prepare
            .store
            .claim_agent_command(ClaimAgentCommand {
                session_id: after_prepare.session_id.clone(),
                task_id: after_prepare.task_id.clone(),
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
        let harness_profile = profile();
        let prepared_id = kiteframe::harness::HarnessInvocationId::new(Uuid::now_v7());
        let prepared = after_prepare
            .store
            .prepare_harness_invocation(PrepareHarnessInvocation {
                invocation_id: &prepared_id,
                command_id: &claimed.id,
                command_revision: claimed.revision,
                profile_id: &harness_profile.id,
                profile_digest: &harness_profile.digest().unwrap(),
                payload_digest: &claimed.payload_digest,
            })
            .await
            .unwrap();
        drop(first_dispatcher);
        let restart_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (restarted, observation) = after_prepare
            .dispatcher("core-success.json", restart_barrier)
            .await;
        let (page_a, page_b) = tokio::join!(
            restarted.recover_harness_invocations(1),
            restarted.recover_harness_invocations(1),
        );
        assert_eq!(
            page_a.unwrap().retryable_pre_accept + page_b.unwrap().retryable_pre_accept,
            1,
            "concurrent recovery calls serialize and count the prepared row once"
        );
        assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
        let completed = after_prepare.invocation().await.unwrap();
        assert_eq!(completed.invocation_id, prepared.invocation_id);
        assert_eq!(completed.profile_id, prepared.profile_id);
        assert_eq!(prompt_count(&observation), 1);

        let Some(mut retryable_loss) = RecoveryFixture::new().await else {
            return;
        };
        let first_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "initialize",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (first_dispatcher, first_observation) = retryable_loss
            .dispatcher("core-success.json", first_barrier.clone())
            .await;
        first_barrier.arm();
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        first_barrier.wait_until_entered().await;
        let crashed = retryable_loss.invocation().await.unwrap();
        first.abort();
        first_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());
        let failed_once = retryable_loss
            .store
            .record_harness_retryable_failure(
                &crashed.invocation_id,
                crashed.attempt,
                1,
                "sha256:integrated-process-loss",
                &json!({"type":"failed","code":"harness_failed","detail":"process lost before acceptance"}),
            )
            .await
            .unwrap();
        assert_eq!(failed_once.state, HarnessInvocationState::Started);
        assert_eq!(failed_once.attempt, 1);
        assert_eq!(prompt_count(&first_observation), 0);
        let restart_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (restarted, second_observation) = retryable_loss
            .dispatcher("core-success.json", restart_barrier)
            .await;
        assert_eq!(
            restarted
                .recover_harness_invocations(1)
                .await
                .unwrap()
                .retryable_pre_accept,
            1
        );
        assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
        let completed = retryable_loss.invocation().await.unwrap();
        assert_eq!(completed.invocation_id, failed_once.invocation_id);
        assert_eq!(completed.attempt, 1, "retry increments exactly once");
        assert_eq!(prompt_count(&second_observation), 1);
        before_insert.cleanup().await;
        after_prepare.cleanup().await;
        retryable_loss.cleanup().await;
    }

    #[tokio::test]
    async fn dispatcher_process_restart_reuses_preaccept_and_never_reprompts_postaccept() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        for (method, point, post_accept) in [
            ("initialize", AcpTestBarrierPoint::BeforeWrite, false),
            ("session/new", AcpTestBarrierPoint::BeforeWrite, false),
            ("acceptance", AcpTestBarrierPoint::BeforeAcceptance, false),
            ("session/prompt", AcpTestBarrierPoint::BeforeWrite, true),
            (
                "session/prompt",
                AcpTestBarrierPoint::BeforeAcknowledgement,
                true,
            ),
            (
                "adapter/shutdown",
                AcpTestBarrierPoint::BeforeAdapterShutdown,
                true,
            ),
        ] {
            let Some(mut recovery) = RecoveryFixture::new().await else {
                return;
            };
            let authority_before = recovery.authority().await;
            let barrier = Arc::new(AcpTestBarrier::new_disarmed(method, point));
            let (dispatcher, first_observation) = recovery
                .dispatcher("core-success.json", barrier.clone())
                .await;
            barrier.arm();
            let first = tokio::spawn(async move { dispatcher.tick_agent_commands(8).await });
            barrier.wait_until_entered().await;
            let crashed = recovery.invocation().await.unwrap();
            first.abort();
            barrier.release();
            assert!(first.await.unwrap_err().is_cancelled());

            let restart_barrier = Arc::new(AcpTestBarrier::new_disarmed(
                "unused",
                AcpTestBarrierPoint::BeforeWrite,
            ));
            let (restarted, second_observation) = recovery
                .dispatcher("core-success.json", restart_barrier)
                .await;
            let report = restarted.recover_harness_invocations(1).await.unwrap();
            let recovered = recovery.invocation().await.unwrap();
            assert_eq!(recovered.invocation_id, crashed.invocation_id);
            assert_eq!(recovered.profile_id, crashed.profile_id);
            if post_accept {
                assert_eq!(report.outcome_unknown, 1);
                assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 0);
                assert_eq!(prompt_count(&second_observation), 0);
                assert_eq!(recovery.authority().await, authority_before);
            } else {
                assert_eq!(report.retryable_pre_accept, 1);
                assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
                let completed = recovery.invocation().await.unwrap();
                assert_eq!(completed.invocation_id, crashed.invocation_id);
                assert_eq!(completed.attempt, crashed.attempt);
                assert_eq!(prompt_count(&second_observation), 1);
            }
            assert!(prompt_count(&first_observation) <= 1);
            recovery.cleanup().await;
        }
    }

    #[tokio::test]
    async fn kernel_completion_before_controlled_shutdown_recovers_one_stable_result() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "adapter/shutdown",
            AcpTestBarrierPoint::BeforeAdapterShutdown,
        ));
        let (dispatcher, first_observation) = recovery
            .dispatcher("core-success.json", barrier.clone())
            .await;
        barrier.arm();
        let first = tokio::spawn(async move { dispatcher.tick_agent_commands(8).await });
        barrier.wait_until_entered().await;
        let invocation = recovery.invocation().await.unwrap();
        let command = recovery.pending_or_owned_command().await;
        recovery
            .kernel
            .complete_command(
                &recovery.principal,
                CompleteAgentCommand {
                    session_id: recovery.session_id.clone(),
                    task_id: recovery.task_id.clone(),
                    command_id: command.id.clone(),
                    expected_command_revision: command.revision,
                    idempotency_key: IdempotencyKey::new(format!(
                        "harness-result:{}",
                        invocation.invocation_id.as_uuid()
                    ))
                    .unwrap(),
                    event: RuntimeEvent::HarnessResult {
                        event_id: format!("harness-result:{}", invocation.invocation_id.as_uuid()),
                        command_key: RuntimeCommandKey::new(command.key.as_str()).unwrap(),
                        outcome: "ok".to_owned(),
                        fields: json!({"answer":"stable"}),
                        evidence_id: EvidenceId::new(invocation.invocation_id.as_uuid()),
                        history: vec![],
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(recovery.authority().await, (GoalPhase::Active, 1));
        first.abort();
        barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());

        let restart_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (restarted, second_observation) = recovery
            .dispatcher("core-success.json", restart_barrier)
            .await;
        let report = restarted.recover_harness_invocations(1).await.unwrap();
        assert_eq!(report.terminal, 1);
        let settled = recovery.invocation().await.unwrap();
        assert_eq!(settled.state, HarnessInvocationState::Completed);
        assert_eq!(settled.invocation_id, invocation.invocation_id);
        assert_eq!(recovery.authority().await, (GoalPhase::Active, 1));
        assert_eq!(prompt_count(&first_observation), 1);
        assert_eq!(prompt_count(&second_observation), 0);
        assert_eq!(
            restarted.recover_harness_invocations(1).await.unwrap(),
            kiteframe::HarnessRecoveryReport::default()
        );
        recovery.cleanup().await;
    }
}
