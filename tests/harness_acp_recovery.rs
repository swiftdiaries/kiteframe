use kiteframe::HarnessRecoveryReport;

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
        fixture
            .store
            .settle_harness_invocation(
                &committed.invocation_id,
                0,
                HarnessInvocationState::Completed,
                &committed_terminal_key,
                &json!({"type":"completed","outcome":"ok","fields":{"answer":"once"},"history":[]}),
            )
            .await
            .unwrap();

        let mut excluded = Vec::new();
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
        assert_eq!(unknown.state, HarnessInvocationState::OutcomeUnknown);
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

    const SOURCE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

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

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp")
            .join(name)
    }

    fn profile() -> HarnessProfile {
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

    fn limits() -> AcpLimits {
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
