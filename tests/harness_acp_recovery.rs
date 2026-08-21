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
    use kiteframe::{AgentCommandId, CreateSession, PostgresStore, PrincipalId, canonical_sha256};
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
                &format!("harness-terminal:{}", retryable.invocation_id.as_uuid()),
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
        let result = json!({
            "type":"harness_result",
            "payload":{
                "event_id":format!("harness-result:{}", committed.invocation_id.as_uuid()),
                "command_key":"harness:recovery:committed",
                "outcome":"ok",
                "fields":{"answer":"once"},
                "evidence_id":committed.invocation_id.as_uuid(),
                "history":[]
            }
        });
        let session_id: Uuid =
            sqlx::query_scalar("SELECT session_id FROM user_tasks WHERE id = $1")
                .bind(fixture.task_id)
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
        let completion_digest = canonical_sha256(&json!({
            "sessionId":session_id,
            "taskId":fixture.task_id,
            "commandId":committed.command_id.as_uuid(),
            "expectedCommandRevision":0,
            "event":&result,
        }))
        .unwrap();
        sqlx::query(
            "UPDATE agent_commands SET state = 'completed', revision = revision + 1, \
             completion_idempotency_key = $1, completion_request_digest = $2, \
             result = $3 WHERE id = $4",
        )
        .bind(format!(
            "harness-result:{}",
            committed.invocation_id.as_uuid()
        ))
        .bind(completion_digest)
        .bind(result)
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
        assert_eq!(retry.state, HarnessInvocationState::Prepared);
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
        HarnessProfileId, HarnessRegistry, HarnessTerminal, HarnessTransport,
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
        assert!(
            !task.is_finished(),
            "the direct registry API retains lifecycle ownership through controlled shutdown"
        );
        shutdown_barrier.release();
        let terminal = task.await.unwrap();
        assert!(matches!(terminal, HarnessTerminal::Completed(_)));
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
        limits_with_attempts(3)
    }

    pub(super) fn limits_with_attempts(max_pre_accept_attempts: u32) -> AcpLimits {
        AcpLimits {
            max_frame_bytes: 64 * 1024,
            shutdown_grace: Duration::from_millis(250),
            max_pre_accept_attempts,
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
    use kiteframe::store::{HarnessInvocationState, HarnessPluginRegistration};
    use kiteframe::{
        AgentCommandDispatcher, AgentCommandId, AgentKernel, AgentSelector, CreateSession,
        GoalContract, GoalPhase, HarnessDispatcherTestBarrier, HarnessDispatcherTestBarrierPoint,
        HarnessDispatcherTestFault, HarnessDispatcherTestFaultPoint, IdempotencyKey, PrincipalId,
        RuntimeCeilings, StartAgent, SubmitUserTask, SuccessCriterion, TaskOwnerAuthorizer,
        TaskPreferences, TaskTarget, ToolCatalogSnapshot, UserTaskId,
    };
    use serde_json::Value;
    use sqlx::postgres::PgPoolOptions;
    use time::{Duration, OffsetDateTime};
    use uuid::Uuid;

    use super::fake_acp_boundaries::{
        SOURCE_REVISION, fixture, limits, limits_with_attempts, profile,
    };

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
            self.dispatcher_with_barriers(script, None, barrier, None)
                .await
        }

        async fn dispatcher_with_barriers(
            &mut self,
            registration_script: &str,
            invocation_script: Option<&str>,
            acp_barrier: Arc<AcpTestBarrier>,
            dispatcher_barrier: Option<Arc<HarnessDispatcherTestBarrier>>,
        ) -> (Arc<AgentCommandDispatcher>, PathBuf) {
            self.configured_dispatcher(
                registration_script,
                invocation_script,
                acp_barrier,
                dispatcher_barrier,
                limits(),
                None,
            )
            .await
        }

        async fn dispatcher_with_attempt_limit(
            &mut self,
            registration_script: &str,
            invocation_script: Option<&str>,
            acp_barrier: Arc<AcpTestBarrier>,
            dispatcher_barrier: Option<Arc<HarnessDispatcherTestBarrier>>,
            max_pre_accept_attempts: u32,
            process_log: Option<&std::path::Path>,
        ) -> (Arc<AgentCommandDispatcher>, PathBuf) {
            self.configured_dispatcher(
                registration_script,
                invocation_script,
                acp_barrier,
                dispatcher_barrier,
                limits_with_attempts(max_pre_accept_attempts),
                process_log,
            )
            .await
        }

        async fn dispatcher_with_fault(
            &mut self,
            registration_script: &str,
            invocation_script: Option<&str>,
            acp_barrier: Arc<AcpTestBarrier>,
            fault: Arc<HarnessDispatcherTestFault>,
            process_log: &std::path::Path,
        ) -> (Arc<AgentCommandDispatcher>, PathBuf) {
            let (dispatcher, observation) = self
                .configured_dispatcher(
                    registration_script,
                    invocation_script,
                    acp_barrier,
                    None,
                    limits(),
                    Some(process_log),
                )
                .await;
            dispatcher.install_test_fault(fault);
            (dispatcher, observation)
        }

        async fn configured_dispatcher(
            &mut self,
            registration_script: &str,
            invocation_script: Option<&str>,
            acp_barrier: Arc<AcpTestBarrier>,
            dispatcher_barrier: Option<Arc<HarnessDispatcherTestBarrier>>,
            acp_limits: kiteframe::harness::AcpLimits,
            process_log: Option<&std::path::Path>,
        ) -> (Arc<AgentCommandDispatcher>, PathBuf) {
            self.observation_index += 1;
            let observation = self
                .workspace
                .join(format!("observation-{}.json", self.observation_index));
            let script = self
                .workspace
                .join(format!("script-{}.json", self.observation_index));
            std::fs::copy(fixture(registration_script), &script).unwrap();
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
            let mut environment = BTreeMap::from([
                (
                    OsString::from("KITEFRAME_FAKE_ACP_SCRIPT"),
                    script.as_os_str().to_owned(),
                ),
                (
                    OsString::from("KITEFRAME_FAKE_ACP_OBSERVATION"),
                    observation.as_os_str().to_owned(),
                ),
                (
                    OsString::from("KITEFRAME_FAKE_ACP_ALLOW_REGISTRATION_EOF"),
                    OsString::from("1"),
                ),
            ]);
            if let Some(process_log) = process_log {
                environment.insert(
                    OsString::from("KITEFRAME_FAKE_ACP_PROCESS_LOG"),
                    process_log.as_os_str().to_owned(),
                );
            }
            let plugin = AcpHarnessPlugin::with_environment(
                harness_profile.clone(),
                AcpCommand {
                    program: PathBuf::from(env!("CARGO_BIN_EXE_kiteframe-fake-acp")),
                    args: vec![],
                    source_revision: SOURCE_REVISION.to_owned(),
                },
                acp_limits,
                environment,
            )
            .with_test_barrier(acp_barrier);
            let pre_accept_limit = plugin.pre_accept_attempt_limit();
            let mut registry = HarnessRegistry::new();
            registry
                .register_with_pre_accept_attempt_limit(Arc::new(plugin), pre_accept_limit)
                .await
                .unwrap();
            if let Some(invocation_script) = invocation_script {
                std::fs::copy(fixture(invocation_script), &script).unwrap();
            }
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
            let dispatcher = kernel.with_harness_registry_routes(
                Arc::new(registry),
                Arc::new(DenyAllHarnessApprovals),
                routes,
            );
            let dispatcher = if let Some(barrier) = dispatcher_barrier {
                dispatcher.with_test_barrier(barrier)
            } else {
                dispatcher
            };
            (Arc::new(dispatcher), observation)
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

    fn prompt_text(path: &PathBuf) -> Option<String> {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["promptText"].as_str().map(str::to_owned))
    }

    fn assert_same_request_identity(
        expected: &kiteframe::store::HarnessInvocationView,
        actual: &kiteframe::store::HarnessInvocationView,
    ) {
        assert_eq!(actual.invocation_id, expected.invocation_id);
        assert_eq!(actual.command_id, expected.command_id);
        assert_eq!(actual.command_revision, expected.command_revision);
        assert_eq!(actual.profile_id, expected.profile_id);
        assert_eq!(actual.profile_digest, expected.profile_digest);
        assert_eq!(actual.payload_digest, expected.payload_digest);
    }

    fn process_log_counts(path: &std::path::Path) -> (usize, usize) {
        let events = std::fs::read_to_string(path).unwrap_or_default();
        let events = events
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let pre_accept_exits = events
            .iter()
            .filter(|event| {
                event["event"] == "start" && event["firstExitAfterRead"] == Value::Bool(true)
            })
            .count();
        let sessions = events
            .iter()
            .filter(|event| event["event"] == "method" && event["method"] == "session/new")
            .count();
        let prompts = events
            .iter()
            .filter(|event| event["event"] == "method" && event["method"] == "session/prompt")
            .count();
        (pre_accept_exits + sessions, prompts)
    }

    #[tokio::test]
    async fn total_pre_accept_attempt_limits_bound_process_and_prompt_counts() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        for max_pre_accept_attempts in [1_u32, 3] {
            let Some(mut recovery) = RecoveryFixture::new().await else {
                return;
            };
            let process_log = recovery.workspace.join("attempt-processes.jsonl");
            let (dispatcher, _) = recovery
                .dispatcher_with_attempt_limit(
                    "core-success.json",
                    Some("preaccept-process-death.json"),
                    Arc::new(AcpTestBarrier::new_disarmed(
                        "unused",
                        AcpTestBarrierPoint::BeforeWrite,
                    )),
                    None,
                    max_pre_accept_attempts,
                    Some(&process_log),
                )
                .await;
            let mut admitted = Vec::new();
            for _ in 0..max_pre_accept_attempts {
                admitted.push(dispatcher.tick_agent_commands(8).await.unwrap());
            }
            let after_budget = dispatcher.tick_agent_commands(8).await.unwrap();
            let terminal = recovery.invocation().await.unwrap();
            let process_counts = process_log_counts(&process_log);
            recovery.cleanup().await;

            assert_eq!(admitted, vec![1; max_pre_accept_attempts as usize]);
            assert_eq!(after_budget, 0, "limit {max_pre_accept_attempts}");
            assert_eq!(terminal.state, HarnessInvocationState::Failed);
            assert_eq!(terminal.attempt, u64::from(max_pre_accept_attempts - 1));
            assert_eq!(
                process_counts,
                (max_pre_accept_attempts as usize, 0),
                "limit {max_pre_accept_attempts} must bound ACP invocation processes and prompts",
            );
        }
    }

    #[tokio::test]
    async fn restart_configuration_cannot_raise_the_persisted_attempt_limit() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let prepare_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterPrepare,
        ));
        let (first_dispatcher, _) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(prepare_barrier.clone()),
                1,
                None,
            )
            .await;
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        prepare_barrier.wait_until_entered().await;
        let prepared = recovery.invocation().await.unwrap();
        first.abort();
        prepare_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());
        let stored_limit: i64 = sqlx::query_scalar(
            "SELECT max_pre_accept_attempts FROM harness_invocations WHERE invocation_id = $1",
        )
        .bind(prepared.invocation_id.as_uuid())
        .fetch_one(recovery.store.pool())
        .await
        .unwrap();
        assert_eq!(stored_limit, 1);

        let process_log = recovery.workspace.join("immutable-limit-processes.jsonl");
        let (restarted, _) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                Some("preaccept-process-death.json"),
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                None,
                3,
                Some(&process_log),
            )
            .await;
        let report = restarted.recover_harness_invocations(1).await.unwrap();
        let admitted = restarted.tick_agent_commands(8).await.unwrap();
        let after_budget = restarted.tick_agent_commands(8).await.unwrap();
        let terminal = recovery.invocation().await.unwrap();
        let process_counts = process_log_counts(&process_log);
        recovery.cleanup().await;

        assert_eq!(report.retryable_pre_accept, 1);
        assert_eq!(admitted, 1);
        assert_eq!(after_budget, 0);
        assert_eq!(terminal.state, HarnessInvocationState::Failed);
        assert_eq!(terminal.attempt, 0);
        assert_eq!(process_counts, (1, 0));
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
        let journal_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::BeforeJournalInsert,
        ));
        let (first_dispatcher, _) = before_insert
            .dispatcher_with_barriers(
                "core-success.json",
                None,
                barrier,
                Some(journal_barrier.clone()),
            )
            .await;
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        journal_barrier.wait_until_entered().await;
        assert!(before_insert.invocation().await.is_none());
        assert_eq!(
            before_insert.pending_or_owned_command().await.state,
            kiteframe::AgentCommandState::Claimed
        );
        first.abort();
        journal_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());
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
        let exact_prompt = prompt_text(&observation).expect("the uninterrupted prompt is observed");

        let Some(mut after_prepare) = RecoveryFixture::new().await else {
            return;
        };
        let registration_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let prepare_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterPrepare,
        ));
        let (first_dispatcher, _) = after_prepare
            .dispatcher_with_barriers(
                "core-success.json",
                None,
                registration_barrier,
                Some(prepare_barrier.clone()),
            )
            .await;
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        prepare_barrier.wait_until_entered().await;
        let prepared = after_prepare.invocation().await.unwrap();
        assert_eq!(prepared.state, HarnessInvocationState::Prepared);
        first.abort();
        prepare_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());
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
        assert_same_request_identity(&prepared, &completed);
        assert_eq!(prompt_count(&observation), 1);
        assert_eq!(
            prompt_text(&observation).as_deref(),
            Some(exact_prompt.as_str())
        );

        let Some(mut retryable_loss) = RecoveryFixture::new().await else {
            return;
        };
        let first_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let retry_commit_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterRetryCommit,
        ));
        let (first_dispatcher, first_observation) = retryable_loss
            .dispatcher_with_barriers(
                "core-success.json",
                Some("preaccept-process-death.json"),
                first_barrier,
                Some(retry_commit_barrier.clone()),
            )
            .await;
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        retry_commit_barrier.wait_until_entered().await;
        let failed_once = retryable_loss.invocation().await.unwrap();
        first.abort();
        retry_commit_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());
        assert_eq!(failed_once.state, HarnessInvocationState::Prepared);
        assert_eq!(failed_once.attempt, 1);
        assert_eq!(prompt_count(&first_observation), 0);
        let restart_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (restarted, second_observation) = retryable_loss
            .dispatcher("core-success.json", restart_barrier)
            .await;
        let first_page = restarted.recover_harness_invocations(1).await.unwrap();
        assert_eq!(first_page.retryable_pre_accept, 1);
        assert_eq!(
            restarted.recover_harness_invocations(1).await.unwrap(),
            kiteframe::HarnessRecoveryReport::default(),
            "the committed retry increment is counted only once"
        );
        assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
        let completed = retryable_loss.invocation().await.unwrap();
        assert_same_request_identity(&failed_once, &completed);
        assert_eq!(completed.attempt, 1, "retry increments exactly once");
        assert_eq!(prompt_count(&second_observation), 1);
        assert_eq!(
            prompt_text(&second_observation).as_deref(),
            Some(exact_prompt.as_str())
        );

        let Some(mut exhausted_cancel) = RecoveryFixture::new().await else {
            return;
        };
        let first_retry_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterRetryCommit,
        ));
        let (first_retry, first_observation) = exhausted_cancel
            .dispatcher_with_attempt_limit(
                "core-success.json",
                Some("preaccept-process-death.json"),
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(first_retry_barrier.clone()),
                2,
                None,
            )
            .await;
        let first_retry = tokio::spawn(async move { first_retry.tick_agent_commands(8).await });
        first_retry_barrier.wait_until_entered().await;
        let incremented = exhausted_cancel.invocation().await.unwrap();
        assert_eq!(incremented.attempt, 1);
        assert!(!incremented.terminal);
        assert_eq!(prompt_count(&first_observation), 0);
        first_retry_barrier.release();
        first_retry.await.unwrap().unwrap();

        let exhausted_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterRetryCommit,
        ));
        let (exhausted, second_observation) = exhausted_cancel
            .dispatcher_with_attempt_limit(
                "core-success.json",
                Some("preaccept-process-death.json"),
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(exhausted_barrier.clone()),
                2,
                None,
            )
            .await;
        let exhausted = tokio::spawn(async move { exhausted.tick_agent_commands(8).await });
        exhausted_barrier.wait_until_entered().await;
        let waiting_for_authority = exhausted_cancel.invocation().await.unwrap();
        assert_eq!(waiting_for_authority.attempt, 1);
        assert!(!waiting_for_authority.terminal);
        let authority = exhausted_cancel
            .store
            .get_agent_execution(
                exhausted_cancel.session_id.clone(),
                exhausted_cancel.task_id.clone(),
            )
            .await
            .unwrap();
        exhausted_cancel
            .kernel
            .cancel_task(
                &exhausted_cancel.principal,
                kiteframe::CancelUserTask {
                    session_id: exhausted_cancel.session_id.clone(),
                    task_id: exhausted_cancel.task_id.clone(),
                    expected_task_revision: authority.task_revision,
                    expected_goal_revision: authority.goal_revision,
                    reason: Some("Cancellation wins exhausted retry.".to_owned()),
                    idempotency_key: IdempotencyKey::new(format!(
                        "cancel-exhausted-{}",
                        exhausted_cancel.task_id.as_uuid()
                    ))
                    .unwrap(),
                },
            )
            .await
            .unwrap();
        exhausted_barrier.release();
        exhausted.await.unwrap().unwrap();
        let cancelled = exhausted_cancel.invocation().await.unwrap();
        assert_eq!(cancelled.state, HarnessInvocationState::Cancelled);
        assert_ne!(cancelled.state, HarnessInvocationState::Failed);
        assert_eq!(prompt_count(&second_observation), 0);
        before_insert.cleanup().await;
        after_prepare.cleanup().await;
        retryable_loss.cleanup().await;
        exhausted_cancel.cleanup().await;
    }

    #[tokio::test]
    async fn exhausted_retry_crash_recovers_terminal_without_exceeding_the_acp_budget() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };

        let process_log = recovery.workspace.join("exhaustion-processes.jsonl");
        let exhausted_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterRetryCommit,
        ));
        let (exhausted_dispatcher, first_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                Some("preaccept-process-death.json"),
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(exhausted_barrier.clone()),
                1,
                Some(&process_log),
            )
            .await;
        let exhausted =
            tokio::spawn(async move { exhausted_dispatcher.tick_agent_commands(8).await });
        exhausted_barrier.wait_until_entered().await;
        let closed_before_command = recovery.invocation().await.unwrap();
        let marker_before_crash: bool = sqlx::query_scalar(
            "SELECT pre_accept_exhausted FROM harness_invocations WHERE invocation_id = $1",
        )
        .bind(closed_before_command.invocation_id.as_uuid())
        .fetch_one(recovery.store.pool())
        .await
        .unwrap();
        exhausted.abort();
        exhausted_barrier.release();
        assert!(exhausted.await.unwrap_err().is_cancelled());

        let restart_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (restarted, restart_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                restart_barrier,
                None,
                1,
                Some(&process_log),
            )
            .await;
        let report = restarted.recover_harness_invocations(1).await.unwrap();
        let recovered_tick = restarted.tick_agent_commands(8).await.unwrap();
        let terminal = recovery.invocation().await.unwrap();
        let process_counts = process_log_counts(&process_log);
        let first_prompts = prompt_count(&first_observation);
        let restart_prompts = prompt_count(&restart_observation);
        let restart_methods = std::fs::read(&restart_observation)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|value| value["methods"].as_array().cloned())
            .unwrap_or_default();
        recovery.cleanup().await;

        assert_eq!(closed_before_command.attempt, 0);
        assert!(!closed_before_command.terminal);
        assert!(
            marker_before_crash,
            "the retryable result closes dispatch durably"
        );
        assert_eq!(first_prompts, 0);
        assert_eq!(report.terminal, 1);
        assert_eq!(report.retryable_pre_accept, 0);
        assert_eq!(report.outcome_unknown, 0);
        assert_eq!(recovered_tick, 0);
        assert_eq!(terminal.state, HarnessInvocationState::Failed);
        assert_eq!(terminal.attempt, 0);
        assert_eq!(restart_prompts, 0);
        assert_eq!(process_counts, (1, 0));
        assert_eq!(
            restart_methods,
            vec![Value::String("initialize".to_owned())]
        );
    }

    #[tokio::test]
    async fn direct_dispatch_settles_committed_exhaustion_without_restart_or_launch() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };

        let process_log = recovery.workspace.join("inline-exhaustion-processes.jsonl");
        let exhausted_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterRetryCommit,
        ));
        let (first_dispatcher, first_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                Some("preaccept-process-death.json"),
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(exhausted_barrier.clone()),
                1,
                Some(&process_log),
            )
            .await;
        let exhausting = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        exhausted_barrier.wait_until_entered().await;
        let exhausted = recovery.invocation().await.unwrap();
        let command_before = recovery.pending_or_owned_command().await;
        let exhaustion_receipt: (bool, Option<String>, Option<Value>) = sqlx::query_as(
            "SELECT pre_accept_exhausted, exhaustion_request_digest, exhaustion_payload \
             FROM harness_invocations WHERE invocation_id = $1",
        )
        .bind(exhausted.invocation_id.as_uuid())
        .fetch_one(recovery.store.pool())
        .await
        .unwrap();
        let process_counts_before = process_log_counts(&process_log);
        exhausting.abort();
        exhausted_barrier.release();
        assert!(exhausting.await.unwrap_err().is_cancelled());

        let (direct, direct_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                None,
                1,
                Some(&process_log),
            )
            .await;
        let first_tick = direct.tick_agent_commands(8).await.unwrap();
        let terminal = recovery.invocation().await.unwrap();
        let command = recovery.pending_or_owned_command().await;
        let command_result: Option<Value> =
            sqlx::query_scalar("SELECT result FROM agent_commands WHERE id = $1")
                .bind(command.id.as_uuid())
                .fetch_one(recovery.store.pool())
                .await
                .unwrap();
        let second_tick = direct.tick_agent_commands(8).await.unwrap();
        let replay = recovery.invocation().await.unwrap();
        let process_counts_after = process_log_counts(&process_log);
        let first_prompts = prompt_count(&first_observation);
        let direct_prompts = prompt_count(&direct_observation);
        recovery.cleanup().await;

        let terminal_key = format!("harness-terminal:{}", exhausted.invocation_id.as_uuid());
        let terminal_payload = serde_json::json!({
            "type": "failed",
            "code": "harness_failed",
            "detail": "harness pre-accept attempt budget exhausted",
        });
        assert_eq!(exhausted.state, HarnessInvocationState::Started);
        assert!(!exhausted.terminal);
        assert_eq!(command_before.state, kiteframe::AgentCommandState::Claimed);
        assert_eq!(
            exhaustion_receipt,
            (
                true,
                Some(terminal_key.clone()),
                Some(terminal_payload.clone())
            )
        );
        assert_eq!(process_counts_before, (1, 0));
        assert_eq!(first_tick, 1);
        assert_eq!(terminal.state, HarnessInvocationState::Failed);
        assert!(terminal.terminal);
        assert_eq!(terminal.terminal_request_digest, Some(terminal_key));
        assert_eq!(terminal.terminal_payload, Some(terminal_payload));
        assert_eq!(command.state, kiteframe::AgentCommandState::Completed);
        assert_eq!(command_result.as_ref().unwrap()["type"], "command_failed");
        assert_eq!(
            command_result.as_ref().unwrap()["payload"]["code"],
            "harness_failed"
        );
        assert_eq!(second_tick, 0);
        assert_eq!(replay.state, HarnessInvocationState::Failed);
        assert_eq!(
            replay.terminal_request_digest,
            terminal.terminal_request_digest
        );
        assert_eq!(replay.terminal_payload, terminal.terminal_payload);
        assert_eq!(process_counts_after, process_counts_before);
        assert_eq!(first_prompts, 0);
        assert_eq!(direct_prompts, 0);
    }

    #[tokio::test]
    async fn recovery_exhausts_stale_started_at_limit_without_launching() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let prepare_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterPrepare,
        ));
        let (first_dispatcher, _) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(prepare_barrier.clone()),
                1,
                None,
            )
            .await;
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        prepare_barrier.wait_until_entered().await;
        let prepared = recovery.invocation().await.unwrap();
        first.abort();
        prepare_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());
        recovery
            .store
            .mark_harness_invocation_started(&prepared.invocation_id, 0)
            .await
            .unwrap();

        let process_log = recovery.workspace.join("preflight-processes.jsonl");
        let (restarted, _) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                None,
                1,
                Some(&process_log),
            )
            .await;
        let report = restarted.recover_harness_invocations(1).await.unwrap();
        let recovered_tick = restarted.tick_agent_commands(8).await.unwrap();
        let terminal = recovery.invocation().await.unwrap();
        let process_counts = process_log_counts(&process_log);
        recovery.cleanup().await;

        assert_eq!(report.terminal, 1);
        assert_eq!(report.retryable_pre_accept, 0);
        assert_eq!(report.outcome_unknown, 0);
        assert_eq!(recovered_tick, 0);
        assert_eq!(terminal.state, HarnessInvocationState::Failed);
        assert_eq!(terminal.attempt, 0);
        assert_eq!(process_counts, (0, 0));
    }

    #[tokio::test]
    async fn crash_after_reservation_before_spawn_consumes_the_only_slot() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let process_log = recovery
            .workspace
            .join("reserved-before-spawn-processes.jsonl");
        let reservation_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterAttemptReservation,
        ));
        let (first_dispatcher, _) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(reservation_barrier.clone()),
                1,
                Some(&process_log),
            )
            .await;
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        reservation_barrier.wait_until_entered().await;
        let reserved = recovery.invocation().await.unwrap();
        assert_eq!(reserved.state, HarnessInvocationState::Started);
        assert_eq!(reserved.attempt, 0);
        assert_eq!(process_log_counts(&process_log), (0, 0));
        first.abort();
        reservation_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());

        let (restarted, _) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                None,
                1,
                Some(&process_log),
            )
            .await;
        let report = restarted.recover_harness_invocations(1).await.unwrap();
        assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 0);
        let terminal = recovery.invocation().await.unwrap();
        let process_counts = process_log_counts(&process_log);
        recovery.cleanup().await;

        assert_eq!(report.terminal, 1);
        assert_eq!(report.retryable_pre_accept, 0);
        assert_eq!(terminal.state, HarnessInvocationState::Failed);
        assert_eq!(terminal.attempt, 0);
        assert_eq!(process_counts, (0, 0));
    }

    #[tokio::test]
    async fn accepted_acp_poll_failure_owns_bounded_cancel_and_shutdown_without_duplicate_process()
    {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let process_log = recovery
            .workspace
            .join("owned-poll-failure-processes.jsonl");
        let acp_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "session/prompt",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        acp_barrier.arm();
        let fault = Arc::new(HarnessDispatcherTestFault::new_disarmed([
            HarnessDispatcherTestFaultPoint::CancellationPoll,
        ]));
        let (dispatcher, observation) = recovery
            .dispatcher_with_fault(
                "core-success.json",
                None,
                acp_barrier.clone(),
                fault.clone(),
                &process_log,
            )
            .await;
        let tick = tokio::spawn({
            let dispatcher = dispatcher.clone();
            async move { dispatcher.tick_agent_commands(8).await }
        });
        acp_barrier.wait_until_entered().await;
        let accepted = recovery.invocation().await.unwrap();
        assert_eq!(accepted.state, HarnessInvocationState::Accepted);
        fault.arm();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !fault.fired(HarnessDispatcherTestFaultPoint::CancellationPoll) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancellation polling fault fires while ACP lifecycle is owned");
        acp_barrier.release();

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), tick)
                .await
                .expect("ACP cancel and shutdown stay bounded")
                .expect("dispatcher task joins")
                .unwrap(),
            1
        );
        let terminal = recovery.invocation().await.unwrap();
        let command = recovery.pending_or_owned_command().await;
        let processes = process_log_counts(&process_log);
        let prompts = prompt_count(&observation);
        assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 0);
        recovery.cleanup().await;

        assert_eq!(terminal.state, HarnessInvocationState::OutcomeUnknown);
        assert!(terminal.terminal);
        assert_eq!(command.state, kiteframe::AgentCommandState::Completed);
        assert_eq!(processes, (1, 0));
        assert_eq!(prompts, 0);
    }

    #[tokio::test]
    async fn late_dispatcher_cannot_steal_or_exhaust_a_live_reservation() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        for max_pre_accept_attempts in [1_u32, 3] {
            let Some(mut recovery) = RecoveryFixture::new().await else {
                return;
            };
            let process_log = recovery
                .workspace
                .join(format!("live-owner-{max_pre_accept_attempts}.jsonl"));
            let owner_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
                HarnessDispatcherTestBarrierPoint::AfterAttemptReservation,
            ));
            let (owner, owner_observation) = recovery
                .dispatcher_with_attempt_limit(
                    "core-success.json",
                    None,
                    Arc::new(AcpTestBarrier::new_disarmed(
                        "unused",
                        AcpTestBarrierPoint::BeforeWrite,
                    )),
                    Some(owner_barrier.clone()),
                    max_pre_accept_attempts,
                    Some(&process_log),
                )
                .await;
            let owner = tokio::spawn(async move { owner.tick_agent_commands(8).await });
            owner_barrier.wait_until_entered().await;
            let reserved = recovery.invocation().await.unwrap();
            assert_eq!(reserved.state, HarnessInvocationState::Started);
            assert_eq!(reserved.attempt, 0);
            assert_eq!(process_log_counts(&process_log), (0, 0));

            let (late, late_observation) = recovery
                .dispatcher_with_attempt_limit(
                    "core-success.json",
                    None,
                    Arc::new(AcpTestBarrier::new_disarmed(
                        "unused",
                        AcpTestBarrierPoint::BeforeWrite,
                    )),
                    None,
                    max_pre_accept_attempts,
                    Some(&process_log),
                )
                .await;
            let late_result = late.tick_agent_commands(8).await;
            let after_late = recovery.invocation().await.unwrap();
            let command_after_late = recovery.pending_or_owned_command().await;
            let exhaustion_after_late: bool = sqlx::query_scalar(
                "SELECT pre_accept_exhausted FROM harness_invocations WHERE invocation_id = $1",
            )
            .bind(reserved.invocation_id.as_uuid())
            .fetch_one(recovery.store.pool())
            .await
            .unwrap();
            let processes_after_late = process_log_counts(&process_log);
            let late_prompts = prompt_count(&late_observation);

            owner_barrier.release();
            let owner_result = owner.await.unwrap();
            let terminal = recovery.invocation().await.unwrap();
            let command = recovery.pending_or_owned_command().await;
            let process_counts = process_log_counts(&process_log);
            let owner_prompts = prompt_count(&owner_observation);
            recovery.cleanup().await;

            assert_eq!(late_result.unwrap(), 1);
            assert_eq!(after_late.state, HarnessInvocationState::Started);
            assert_eq!(after_late.attempt, 0);
            assert!(!after_late.terminal);
            assert_eq!(
                command_after_late.state,
                kiteframe::AgentCommandState::Claimed
            );
            assert!(!exhaustion_after_late);
            assert_eq!(processes_after_late, (0, 0));
            assert_eq!(late_prompts, 0);
            assert_eq!(owner_result.unwrap(), 1);
            assert_eq!(terminal.state, HarnessInvocationState::Completed);
            assert_eq!(terminal.attempt, 0);
            assert_eq!(command.state, kiteframe::AgentCommandState::Completed);
            assert_eq!(process_counts, (1, 1));
            assert_eq!(owner_prompts, 1);
        }
    }

    #[tokio::test]
    async fn direct_dispatch_leaves_stale_started_for_quiescent_recovery() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let process_log = recovery.workspace.join("direct-stale-processes.jsonl");
        let crash_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "initialize",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (first_dispatcher, first_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                Some("preaccept-process-death.json"),
                crash_barrier.clone(),
                None,
                3,
                Some(&process_log),
            )
            .await;
        crash_barrier.arm();
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        crash_barrier.wait_until_entered().await;
        let crashed = recovery.invocation().await.unwrap();
        assert_eq!(crashed.state, HarnessInvocationState::Started);
        assert_eq!(crashed.attempt, 0);
        first.abort();
        crash_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());

        let (restarted, second_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                None,
                3,
                Some(&process_log),
            )
            .await;
        assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
        let still_started = recovery.invocation().await.unwrap();
        let command = recovery.pending_or_owned_command().await;
        let process_counts = process_log_counts(&process_log);
        let first_prompts = prompt_count(&first_observation);
        let second_prompts = prompt_count(&second_observation);
        recovery.cleanup().await;

        assert_same_request_identity(&crashed, &still_started);
        assert_eq!(still_started.state, HarnessInvocationState::Started);
        assert_eq!(still_started.attempt, 0);
        assert_eq!(command.state, kiteframe::AgentCommandState::Claimed);
        assert_eq!(first_prompts, 0);
        assert_eq!(second_prompts, 0);
        assert_eq!(process_counts, (1, 0));
    }

    #[tokio::test]
    async fn repeated_spawn_crashes_consume_exactly_the_persisted_attempt_budget() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        for max_pre_accept_attempts in [1_u32, 3] {
            let Some(mut recovery) = RecoveryFixture::new().await else {
                return;
            };
            let process_log = recovery
                .workspace
                .join(format!("restart-budget-{max_pre_accept_attempts}.jsonl"));
            let mut invocation_id = None;

            for ordinal in 0..max_pre_accept_attempts {
                let crash_barrier = Arc::new(AcpTestBarrier::new_disarmed(
                    "initialize",
                    AcpTestBarrierPoint::BeforeWrite,
                ));
                let (dispatcher, observation) = recovery
                    .dispatcher_with_attempt_limit(
                        "core-success.json",
                        Some("preaccept-process-death.json"),
                        crash_barrier.clone(),
                        None,
                        max_pre_accept_attempts,
                        Some(&process_log),
                    )
                    .await;
                if ordinal > 0 {
                    let report = dispatcher.recover_harness_invocations(1).await.unwrap();
                    assert_eq!(report.retryable_pre_accept, 1);
                    assert_eq!(report.terminal, 0);
                    let prepared = recovery.invocation().await.unwrap();
                    assert_eq!(prepared.state, HarnessInvocationState::Prepared);
                    assert_eq!(prepared.attempt, u64::from(ordinal));
                }
                crash_barrier.arm();
                let crashing = tokio::spawn(async move { dispatcher.tick_agent_commands(8).await });
                crash_barrier.wait_until_entered().await;
                let started = recovery.invocation().await.unwrap();
                let expected_id =
                    invocation_id.get_or_insert_with(|| started.invocation_id.clone());
                assert_eq!(&started.invocation_id, expected_id);
                assert_eq!(started.state, HarnessInvocationState::Started);
                assert_eq!(started.attempt, u64::from(ordinal));
                assert_eq!(prompt_count(&observation), 0);
                assert_eq!(
                    process_log_counts(&process_log),
                    (usize::try_from(ordinal + 1).unwrap(), 0)
                );
                crashing.abort();
                crash_barrier.release();
                assert!(crashing.await.unwrap_err().is_cancelled());
            }

            let (restarted, observation) = recovery
                .dispatcher_with_attempt_limit(
                    "core-success.json",
                    None,
                    Arc::new(AcpTestBarrier::new_disarmed(
                        "unused",
                        AcpTestBarrierPoint::BeforeWrite,
                    )),
                    None,
                    max_pre_accept_attempts,
                    Some(&process_log),
                )
                .await;
            let report = restarted.recover_harness_invocations(1).await.unwrap();
            assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 0);
            assert_eq!(
                restarted.recover_harness_invocations(1).await.unwrap(),
                kiteframe::HarnessRecoveryReport::default()
            );
            let terminal = recovery.invocation().await.unwrap();
            let process_counts = process_log_counts(&process_log);
            let restart_prompts = prompt_count(&observation);
            recovery.cleanup().await;

            assert_eq!(report.terminal, 1);
            assert_eq!(report.retryable_pre_accept, 0);
            assert_eq!(terminal.invocation_id, invocation_id.unwrap());
            assert_eq!(terminal.state, HarnessInvocationState::Failed);
            assert_eq!(terminal.attempt, u64::from(max_pre_accept_attempts - 1));
            assert_eq!(
                process_counts,
                (max_pre_accept_attempts as usize, 0),
                "limit {max_pre_accept_attempts} launches no fourth process"
            );
            assert_eq!(restart_prompts, 0);
        }
    }

    #[tokio::test]
    async fn cancellation_racing_stale_advancement_remains_authoritative() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let process_log = recovery.workspace.join("cancel-stale-processes.jsonl");
        let crash_barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "initialize",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (first_dispatcher, first_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                Some("preaccept-process-death.json"),
                crash_barrier.clone(),
                None,
                3,
                Some(&process_log),
            )
            .await;
        crash_barrier.arm();
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        crash_barrier.wait_until_entered().await;
        assert_eq!(
            recovery.invocation().await.unwrap().state,
            HarnessInvocationState::Started
        );
        first.abort();
        crash_barrier.release();
        assert!(first.await.unwrap_err().is_cancelled());

        let (restarted, second_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                None,
                3,
                Some(&process_log),
            )
            .await;
        let authority = recovery
            .store
            .get_agent_execution(recovery.session_id.clone(), recovery.task_id.clone())
            .await
            .unwrap();
        let cancellation = recovery.kernel.cancel_task(
            &recovery.principal,
            kiteframe::CancelUserTask {
                session_id: recovery.session_id.clone(),
                task_id: recovery.task_id.clone(),
                expected_task_revision: authority.task_revision,
                expected_goal_revision: authority.goal_revision,
                reason: Some("Cancellation races stale harness advancement.".to_owned()),
                idempotency_key: IdempotencyKey::new(format!(
                    "cancel-stale-{}",
                    recovery.task_id.as_uuid()
                ))
                .unwrap(),
            },
        );
        let (cancelled, report) =
            tokio::join!(cancellation, restarted.recover_harness_invocations(1));
        cancelled.unwrap();
        let report = report.unwrap();
        assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 0);
        let terminal = recovery.invocation().await.unwrap();
        let command = recovery.pending_or_owned_command().await;
        let process_counts = process_log_counts(&process_log);
        let prompts = prompt_count(&first_observation) + prompt_count(&second_observation);
        recovery.cleanup().await;

        assert!(report.retryable_pre_accept + report.terminal <= 1);
        assert_eq!(terminal.state, HarnessInvocationState::Cancelled);
        assert_eq!(command.state, kiteframe::AgentCommandState::Cancelled);
        assert_eq!(process_counts, (1, 0));
        assert_eq!(prompts, 0);
    }

    #[tokio::test]
    async fn concurrent_dispatchers_reserve_one_process_for_one_prepared_ordinal() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let prepare = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterPrepare,
        ));
        let (preparer, _) = recovery
            .dispatcher_with_barriers(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(prepare.clone()),
            )
            .await;
        let preparing = tokio::spawn(async move { preparer.tick_agent_commands(8).await });
        prepare.wait_until_entered().await;
        assert_eq!(
            recovery.invocation().await.unwrap().state,
            HarnessInvocationState::Prepared
        );
        preparing.abort();
        prepare.release();
        assert!(preparing.await.unwrap_err().is_cancelled());

        let process_log = recovery.workspace.join("reservation-race-processes.jsonl");
        let first_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterPrepare,
        ));
        let second_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterPrepare,
        ));
        let (first_dispatcher, first_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(first_barrier.clone()),
                3,
                Some(&process_log),
            )
            .await;
        let (second_dispatcher, second_observation) = recovery
            .dispatcher_with_attempt_limit(
                "core-success.json",
                None,
                Arc::new(AcpTestBarrier::new_disarmed(
                    "unused",
                    AcpTestBarrierPoint::BeforeWrite,
                )),
                Some(second_barrier.clone()),
                3,
                Some(&process_log),
            )
            .await;
        let first = tokio::spawn(async move { first_dispatcher.tick_agent_commands(8).await });
        let second = tokio::spawn(async move { second_dispatcher.tick_agent_commands(8).await });
        first_barrier.wait_until_entered().await;
        second_barrier.wait_until_entered().await;
        first_barrier.release();
        second_barrier.release();
        assert_eq!(first.await.unwrap().unwrap(), 1);
        assert_eq!(second.await.unwrap().unwrap(), 1);
        let terminal = recovery.invocation().await.unwrap();
        let process_counts = process_log_counts(&process_log);
        let prompts = prompt_count(&first_observation) + prompt_count(&second_observation);
        recovery.cleanup().await;

        assert_eq!(terminal.state, HarnessInvocationState::Completed);
        assert_eq!(terminal.attempt, 0);
        assert_eq!(process_counts, (1, 1));
        assert_eq!(prompts, 1);
    }

    #[tokio::test]
    async fn dispatcher_process_restart_reuses_preaccept_and_never_reprompts_postaccept() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        for (method, point, post_accept, first_prompts, defer_shutdown) in [
            (
                "initialize",
                AcpTestBarrierPoint::BeforeWrite,
                false,
                0,
                false,
            ),
            (
                "session/new",
                AcpTestBarrierPoint::BeforeWrite,
                false,
                0,
                false,
            ),
            (
                "acceptance",
                AcpTestBarrierPoint::BeforeAcceptance,
                false,
                0,
                false,
            ),
            (
                "session/prompt",
                AcpTestBarrierPoint::BeforeWrite,
                true,
                0,
                false,
            ),
            (
                "session/prompt",
                AcpTestBarrierPoint::BeforeAcknowledgement,
                true,
                1,
                false,
            ),
            (
                "adapter/shutdown",
                AcpTestBarrierPoint::BeforeAdapterShutdown,
                true,
                1,
                true,
            ),
        ] {
            let Some(mut recovery) = RecoveryFixture::new().await else {
                return;
            };
            let authority_before = recovery.authority().await;
            let barrier = Arc::new(AcpTestBarrier::new_disarmed(method, point));
            let dispatcher_barrier = defer_shutdown.then(|| {
                Arc::new(HarnessDispatcherTestBarrier::new(
                    HarnessDispatcherTestBarrierPoint::BeforeCommandTerminal,
                ))
            });
            let (dispatcher, first_observation) = recovery
                .dispatcher_with_barriers(
                    "core-success.json",
                    None,
                    barrier.clone(),
                    dispatcher_barrier.clone(),
                )
                .await;
            barrier.arm();
            let first = tokio::spawn(async move { dispatcher.tick_agent_commands(8).await });
            barrier.wait_until_entered().await;
            if let Some(dispatcher_barrier) = &dispatcher_barrier {
                dispatcher_barrier.wait_until_entered().await;
            }
            let crashed = recovery.invocation().await.unwrap();
            first.abort();
            barrier.release();
            if let Some(dispatcher_barrier) = &dispatcher_barrier {
                dispatcher_barrier.release();
            }
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
                assert_eq!(report.outcome_unknown, 1, "{method:?} {point:?}");
                assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 0);
                assert_eq!(prompt_count(&second_observation), 0);
                assert_eq!(recovery.authority().await, authority_before);
            } else {
                assert_eq!(report.retryable_pre_accept, 1, "{method:?} {point:?}");
                assert_eq!(recovered.state, HarnessInvocationState::Prepared);
                assert_eq!(recovered.attempt, crashed.attempt + 1);
                assert_eq!(restarted.tick_agent_commands(8).await.unwrap(), 1);
                let completed = recovery.invocation().await.unwrap();
                assert_eq!(completed.invocation_id, crashed.invocation_id);
                assert_eq!(completed.attempt, crashed.attempt + 1);
                assert_eq!(completed.state, HarnessInvocationState::Completed);
                assert_eq!(prompt_count(&second_observation), 1);
            }
            assert_eq!(prompt_count(&first_observation), first_prompts);
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
        let command_barrier = Arc::new(HarnessDispatcherTestBarrier::new(
            HarnessDispatcherTestBarrierPoint::AfterCommandTerminal,
        ));
        let (dispatcher, first_observation) = recovery
            .dispatcher_with_barriers(
                "core-success.json",
                None,
                barrier.clone(),
                Some(command_barrier.clone()),
            )
            .await;
        barrier.arm();
        let first = tokio::spawn(async move { dispatcher.tick_agent_commands(8).await });
        barrier.wait_until_entered().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            command_barrier.wait_until_entered(),
        )
        .await
        .expect("the real ACP terminal must reach dispatcher command completion before shutdown");
        let invocation = recovery.invocation().await.unwrap();
        let command = recovery.pending_or_owned_command().await;
        assert_eq!(recovery.authority().await, (GoalPhase::Active, 1));
        let receipt_before: Value = sqlx::query_scalar(
            "SELECT jsonb_build_object('state', state, 'key', completion_idempotency_key, \
             'digest', completion_request_digest, 'result', result) \
             FROM agent_commands WHERE id = $1",
        )
        .bind(command.id.as_uuid())
        .fetch_one(recovery.store.pool())
        .await
        .unwrap();
        assert_eq!(receipt_before["state"], "completed");
        assert_eq!(
            receipt_before["result"]["payload"]["fields"]["answer"],
            "done"
        );
        let evidence_before: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM goal_evidence WHERE task_id = $1 ORDER BY id")
                .bind(recovery.task_id.as_uuid())
                .fetch_all(recovery.store.pool())
                .await
                .unwrap();
        assert_eq!(
            evidence_before,
            vec![invocation.invocation_id.as_uuid()],
            "the harness invocation UUID is the one deterministic evidence UUID"
        );
        first.abort();
        barrier.release();
        command_barrier.release();
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
        assert_eq!(settled.profile_id, invocation.profile_id);
        assert_eq!(
            settled.terminal_payload.as_ref().unwrap()["fields"]["answer"],
            "done"
        );
        assert_eq!(recovery.authority().await, (GoalPhase::Active, 1));
        assert_eq!(prompt_count(&first_observation), 1);
        assert_eq!(prompt_count(&second_observation), 0);
        assert_eq!(
            restarted.recover_harness_invocations(1).await.unwrap(),
            kiteframe::HarnessRecoveryReport::default()
        );
        let receipt_after: Value = sqlx::query_scalar(
            "SELECT jsonb_build_object('state', state, 'key', completion_idempotency_key, \
             'digest', completion_request_digest, 'result', result) \
             FROM agent_commands WHERE id = $1",
        )
        .bind(command.id.as_uuid())
        .fetch_one(recovery.store.pool())
        .await
        .unwrap();
        assert_eq!(receipt_after, receipt_before);
        let evidence_after: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM goal_evidence WHERE task_id = $1 ORDER BY id")
                .bind(recovery.task_id.as_uuid())
                .fetch_all(recovery.store.pool())
                .await
                .unwrap();
        assert_eq!(
            evidence_after, evidence_before,
            "restart recovery neither remints nor duplicates evidence"
        );
        recovery.cleanup().await;
    }

    #[tokio::test]
    async fn production_result_handoff_keeps_shutdown_owned_until_the_child_is_reaped() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "adapter/shutdown",
            AcpTestBarrierPoint::BeforeAdapterShutdown,
        ));
        let (dispatcher, observation) = recovery
            .dispatcher_with_barriers("core-success.json", None, barrier.clone(), None)
            .await;
        barrier.arm();
        let tick = tokio::spawn(async move { dispatcher.tick_agent_commands(8).await });
        barrier.wait_until_entered().await;

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if recovery
                    .invocation()
                    .await
                    .is_some_and(|invocation| invocation.state == HarnessInvocationState::Completed)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the production result must settle before subprocess shutdown");
        let finished_before_shutdown_release = tick.is_finished();
        barrier.release();
        let result = tick.await.unwrap();
        assert!(
            !finished_before_shutdown_release,
            "the dispatcher must retain ownership of lifecycle shutdown"
        );
        assert_eq!(result.unwrap(), 1);
        let observation: Value =
            serde_json::from_slice(&std::fs::read(observation).unwrap()).unwrap();
        assert_eq!(observation["scriptComplete"], true);
        recovery.cleanup().await;
    }

    #[tokio::test]
    async fn shutdown_failure_after_result_is_audited_without_rewriting_the_command_winner() {
        let _database = super::RECOVERY_DATABASE_LOCK.lock().await;
        let Some(mut recovery) = RecoveryFixture::new().await else {
            return;
        };
        let barrier = Arc::new(AcpTestBarrier::new_disarmed(
            "unused",
            AcpTestBarrierPoint::BeforeWrite,
        ));
        let (dispatcher, observation) = recovery.dispatcher("shutdown-nonzero.json", barrier).await;
        assert_eq!(dispatcher.tick_agent_commands(8).await.unwrap(), 1);

        let invocation = recovery.invocation().await.unwrap();
        assert_eq!(invocation.state, HarnessInvocationState::Completed);
        assert_eq!(
            invocation.terminal_payload.as_ref().unwrap()["fields"]["answer"],
            "done"
        );
        let command = recovery.pending_or_owned_command().await;
        assert_eq!(command.state, kiteframe::AgentCommandState::Completed);
        assert_eq!(recovery.authority().await, (GoalPhase::Active, 1));
        let audit = sqlx::query(
            "SELECT stage, code, detail FROM harness_invocation_lifecycle_audits \
             WHERE invocation_id = $1",
        )
        .bind(invocation.invocation_id.as_uuid())
        .fetch_one(recovery.store.pool())
        .await
        .unwrap();
        assert_eq!(sqlx::Row::get::<String, _>(&audit, "stage"), "shutdown");
        assert_eq!(
            sqlx::Row::get::<String, _>(&audit, "code"),
            "harness_outcome_unknown"
        );
        let detail = sqlx::Row::get::<String, _>(&audit, "detail");
        assert_eq!(
            detail,
            "ACP process connection lost: ACP process reached EOF"
        );
        assert_eq!(prompt_count(&observation), 1);
        recovery.cleanup().await;
    }
}
