use std::sync::Arc;

use kiteframe::{
    AgentCommandId, CreateSession, PostgresStore, PrincipalId, StoreError,
    harness::{
        HarnessApprovalDecision, HarnessEventEnvelope, HarnessInvocationEvent, HarnessInvocationId,
        HarnessProfileId, HarnessTransport,
    },
    store::{
        HarnessInvocationEventRecord, HarnessInvocationState, HarnessInvocationView,
        HarnessPluginRegistration, PrepareHarnessInvocation,
    },
};
use serde_json::json;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

struct HarnessStoreFixture {
    store: Arc<PostgresStore>,
    pool: PgPool,
    command_id: AgentCommandId,
    transition_id: Uuid,
    profile_id: HarnessProfileId,
    profile_digest: String,
}

impl HarnessStoreFixture {
    async fn new() -> Option<Self> {
        let Some(database_url) = std::env::var("DATABASE_URL").ok() else {
            eprintln!("SKIP harness_store: DATABASE_URL is not set");
            return None;
        };
        let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let principal = PrincipalId::new(format!("harness-store-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal,
                working_directory: "/harness-store".to_owned(),
                retention_until: OffsetDateTime::now_utc() + Duration::days(1),
            })
            .await
            .unwrap();
        let pool = store.pool().clone();
        let definition_id = Uuid::now_v7();
        let task_id = Uuid::now_v7();
        let transition_id = Uuid::now_v7();
        let command_id = AgentCommandId::new(Uuid::now_v7());
        sqlx::query(
            "INSERT INTO workflow_definitions \
             (id, name, version, definition_hash, source, compiled_definition, tool_bindings, definition_kind) \
             VALUES ($1, $2, 1, $3, 'agent example {}', '{}'::jsonb, '[]'::jsonb, 'agent')",
        )
        .bind(definition_id)
        .bind(format!("harness-store-{}", Uuid::now_v7()))
        .bind(format!("sha256:{}", Uuid::now_v7()))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_tasks \
             (id, session_id, principal, definition_id, submission_idempotency_key, request_digest, target, preferences) \
             VALUES ($1, $2, 'harness-store', $3, $4, 'sha256:task', '{}'::jsonb, '{}'::jsonb)",
        )
        .bind(task_id)
        .bind(session.id.as_uuid())
        .bind(definition_id)
        .bind(format!("submit-{}", Uuid::now_v7()))
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
        sqlx::query(
            "INSERT INTO agent_commands \
             (id, task_id, transition_id, ordinal, schema_version, runtime_key, command_kind, payload, payload_digest) \
             VALUES ($1, $2, $3, 0, 1, 'harness:store', 'invoke_harness', $4, 'sha256:command')",
        )
        .bind(command_id.as_uuid())
        .bind(task_id)
        .bind(transition_id)
        .bind(json!({"type": "invoke_harness", "payload": {}}))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE agent_commands SET state = 'claimed', claimant = 'harness-store-test' WHERE id = $1",
        )
        .bind(command_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
        let profile_id = HarnessProfileId::new(format!("test-harness-{}", Uuid::now_v7())).unwrap();
        let profile_digest = "sha256:profile-a".to_owned();
        store
            .register_harness_plugin(HarnessPluginRegistration {
                profile_id: profile_id.clone(),
                profile_digest: profile_digest.clone(),
                source_repository: "https://example.test/harness".to_owned(),
                source_revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                transport: HarnessTransport::AcpStdio,
                enabled: true,
            })
            .await
            .unwrap();
        Some(Self {
            store,
            pool,
            command_id,
            transition_id,
            profile_id,
            profile_digest,
        })
    }

    async fn prepare_with(
        &self,
        invocation_id: &HarnessInvocationId,
        payload_digest: &str,
    ) -> Result<HarnessInvocationView, StoreError> {
        self.prepare_for(&self.command_id, invocation_id, payload_digest)
            .await
    }

    async fn prepare_for(
        &self,
        command_id: &AgentCommandId,
        invocation_id: &HarnessInvocationId,
        payload_digest: &str,
    ) -> Result<HarnessInvocationView, StoreError> {
        self.store
            .prepare_harness_invocation(PrepareHarnessInvocation {
                invocation_id,
                command_id,
                command_revision: 0,
                profile_id: &self.profile_id,
                profile_digest: &self.profile_digest,
                payload_digest,
            })
            .await
    }

    async fn prepare(&self, payload_digest: &str) -> Result<HarnessInvocationView, StoreError> {
        self.prepare_with(&HarnessInvocationId::new(Uuid::now_v7()), payload_digest)
            .await
    }

    async fn additional_command(&self) -> AgentCommandId {
        let command_id = AgentCommandId::new(Uuid::now_v7());
        sqlx::query(
            "INSERT INTO agent_commands \
             (id, task_id, transition_id, ordinal, schema_version, runtime_key, command_kind, payload, payload_digest) \
             SELECT $1, task_id, $2, 1, 1, 'harness:store:second', 'invoke_harness', \
                    $3, 'sha256:command-second' \
             FROM agent_commands WHERE id = $4",
        )
        .bind(command_id.as_uuid())
        .bind(self.transition_id)
        .bind(json!({"type": "invoke_harness", "payload": {}}))
        .bind(self.command_id.as_uuid())
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE agent_commands SET state = 'claimed', claimant = 'harness-store-test' WHERE id = $1",
        )
        .bind(command_id.as_uuid())
        .execute(&self.pool)
        .await
        .unwrap();
        command_id
    }

    fn event(
        &self,
        invocation_id: &HarnessInvocationId,
        sequence: u64,
        event: HarnessInvocationEvent,
    ) -> HarnessInvocationEventRecord {
        HarnessInvocationEventRecord::new(invocation_id.clone(), sequence, event).unwrap()
    }

    async fn append_approval_pair(
        &self,
        invocation_id: &HarnessInvocationId,
        request_id: &str,
        decision: HarnessApprovalDecision,
    ) -> Result<(), StoreError> {
        self.store
            .append_harness_invocation_event(&self.event(
                invocation_id,
                0,
                HarnessInvocationEvent::Started,
            ))
            .await?;
        self.store
            .append_harness_invocation_event(&self.event(
                invocation_id,
                1,
                HarnessInvocationEvent::ApprovalRequested {
                    request_id: request_id.to_owned(),
                    title: "Need permission".to_owned(),
                },
            ))
            .await?;
        self.store
            .append_harness_invocation_event(&self.event(
                invocation_id,
                2,
                HarnessInvocationEvent::ApprovalResolved {
                    request_id: request_id.to_owned(),
                    decision,
                },
            ))
            .await
    }

    async fn exclude_other_invocations(
        &self,
        keep: &[HarnessInvocationId],
    ) -> Vec<HarnessInvocationId> {
        let keep = keep
            .iter()
            .map(HarnessInvocationId::as_uuid)
            .collect::<Vec<_>>();
        sqlx::query_scalar::<_, Uuid>(
            "SELECT invocation_id FROM harness_invocations WHERE NOT (invocation_id = ANY($1))",
        )
        .bind(keep)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(HarnessInvocationId::new)
        .collect()
    }
}

#[test]
fn harness_migration_has_closed_state_and_identity_guards() {
    let sql = std::fs::read_to_string("migrations/0014_harness_plugins.sql")
        .expect("Task 2 harness migration exists");
    for required in [
        "CREATE TABLE harness_plugin_registrations",
        "CREATE TABLE harness_invocations",
        "CREATE TABLE harness_invocation_events",
        "outcome_unknown",
        "reject_harness_invocation_mutation",
        "reject_harness_invocation_event_mutation",
        "BEFORE UPDATE OR DELETE ON harness_invocations",
        "jsonb_typeof(event->'decision') = 'string'",
    ] {
        assert!(sql.contains(required), "missing schema guard: {required}");
    }
}

#[test]
fn terminal_replay_payload_is_immutable_journal_data() {
    let sql = std::fs::read_to_string("migrations/0015_harness_terminal_replay.sql")
        .expect("Task 4 terminal replay migration exists");
    for required in [
        "terminal_payload JSONB",
        "DISABLE TRIGGER harness_invocations_reject_invalid_update",
        "UPDATE harness_invocations",
        "harness-terminal:' || invocation_id::text",
        "terminal_payload IS NOT NULL",
        "terminal harness invocation is immutable",
    ] {
        assert!(
            sql.contains(required),
            "missing terminal replay guard: {required}"
        );
    }
    assert!(
        sql.find("UPDATE harness_invocations").unwrap()
            < sql
                .find("ADD CONSTRAINT harness_invocations_terminal_payload_check")
                .unwrap(),
        "terminal payloads must be backfilled before the constraint is enabled"
    );
}

#[test]
fn gate_a_migration_backfills_v1_event_identity_and_removes_zero_attestation() {
    let sql = std::fs::read_to_string("migrations/0016_harness_gate_a.sql")
        .expect("Gate A migration exists");
    for required in [
        "harness_event_envelope_digest_v1",
        "harness_event_is_closed_v1",
        "jsonb_typeof(event)",
        "event = '{\"type\":\"started\"}'::jsonb",
        "event = '{\"type\":\"cancelled\"}'::jsonb",
        "jsonb_typeof(event->'message') = 'string'",
        "event - 'type' - 'message'",
        "jsonb_typeof(event->'text') = 'string'",
        "event - 'type' - 'text'",
        "jsonb_typeof(event->'requestId') = 'string'",
        "event - 'type' - 'requestId' - 'title'",
        "jsonb_typeof(event->'decision') = 'string'",
        "event - 'type' - 'requestId' - 'decision'",
        "requestId",
        "allow_once",
        "reject_once",
        "harness event is not a closed v1 event",
        "DROP CONSTRAINT harness_invocation_events_event_check",
        "harness_invocation_events_v1_event_check",
        "protocolVersion",
        "invocationId",
        "sequence zero must be Started",
        "harness event sequence gap",
        "harness event replay differs from the durable event",
        "0000000000000000000000000000000000000000",
        "zero-revision builtin registration has durable invocations",
        "harness_plugin_registrations_nonzero_revision_check",
    ] {
        assert!(sql.contains(required), "missing Gate A guard: {required}");
    }
    let registrations_lock = sql
        .find("LOCK TABLE harness_plugin_registrations")
        .expect("registration table lock is explicit");
    let invocations_lock = sql
        .find("LOCK TABLE harness_invocations")
        .expect("invocation table lock is explicit");
    let events_lock = sql
        .find("LOCK TABLE harness_invocation_events")
        .expect("event table lock is explicit");
    assert!(
        registrations_lock < invocations_lock && invocations_lock < events_lock,
        "migration lock order must be registrations, invocations, then events"
    );
}

#[tokio::test]
async fn database_digest_rejects_every_malformed_v1_event_shape() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let invalid_events = [
        json!({"type":"started", "extra":true}),
        json!({"type":"progress", "message":7}),
        json!({"type":"assistant_message", "text":null}),
        json!({"type":"unknown"}),
        json!({
            "type":"approval_resolved",
            "requestId":"permission-7",
            "decision":"allow_forever"
        }),
        json!({"type":"approval_requested", "title":"Need permission"}),
    ];

    for event in invalid_events {
        let error =
            sqlx::query_scalar::<_, String>("SELECT harness_event_envelope_digest_v1($1, $2, $3)")
                .bind(invocation.invocation_id.as_uuid())
                .bind(1_i64)
                .bind(event.clone())
                .fetch_one(&fixture.pool)
                .await
                .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("55000"),
            "malformed event must fail before digest acceptance: {event}"
        );
    }
}

#[tokio::test]
async fn prepare_is_idempotent_for_one_command_revision_and_rejects_drift() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation_id = HarnessInvocationId::new(Uuid::now_v7());
    let prepared = fixture
        .prepare_with(&invocation_id, "sha256:payload-a")
        .await
        .unwrap();
    let replay = fixture
        .prepare_with(&invocation_id, "sha256:payload-a")
        .await
        .unwrap();
    assert_eq!(replay.invocation_id, prepared.invocation_id);
    assert_eq!(replay.state, HarnessInvocationState::Prepared);
    let error = fixture
        .prepare_with(&invocation_id, "sha256:payload-b")
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::IdempotencyConflict));
    let identity_drift = fixture
        .prepare_with(
            &HarnessInvocationId::new(Uuid::now_v7()),
            "sha256:payload-a",
        )
        .await
        .unwrap();
    assert_eq!(identity_drift.invocation_id, prepared.invocation_id);
    let second_command = fixture.additional_command().await;
    let command_drift = fixture
        .prepare_for(&second_command, &invocation_id, "sha256:payload-a")
        .await
        .unwrap_err();
    assert!(matches!(command_drift, StoreError::IdempotencyConflict));
}

#[tokio::test]
async fn terminal_prepare_replay_discards_a_fresh_candidate_and_returns_the_stored_invocation() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let terminal_key = format!("harness-terminal:{}", invocation.invocation_id.as_uuid());
    sqlx::query(
        "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 WHERE id = $1",
    )
    .bind(fixture.command_id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture
        .store
        .settle_harness_invocation(
            &invocation.invocation_id,
            0,
            HarnessInvocationState::Cancelled,
            &terminal_key,
            &json!({"type":"cancelled"}),
        )
        .await
        .unwrap();

    let replay = fixture.prepare("sha256:payload-a").await.unwrap();
    assert_eq!(replay.invocation_id, invocation.invocation_id);
    assert_eq!(replay.state, HarnessInvocationState::Cancelled);
    assert!(replay.terminal);
}

#[tokio::test]
async fn accepted_process_death_is_classified_without_preempting_command_authority() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation.invocation_id, 0)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation.invocation_id, 0)
        .await
        .unwrap();
    let excluded = fixture
        .exclude_other_invocations(std::slice::from_ref(&invocation.invocation_id))
        .await;
    let recovered = fixture
        .store
        .recover_harness_invocations_excluding(16, &excluded)
        .await
        .unwrap();
    let recovered = recovered
        .into_iter()
        .find(|view| view.invocation_id == invocation.invocation_id)
        .expect("accepted invocation is recovered");
    assert_eq!(recovered.state, HarnessInvocationState::Accepted);
    assert!(!recovered.terminal);
    assert_eq!(recovered.terminal_request_digest, None);
    assert_eq!(recovered.terminal_payload, None);
}

#[tokio::test]
async fn recovery_limit_is_total_and_exclusion_makes_bounded_scans_progress() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let retryable_id = HarnessInvocationId::new(Uuid::now_v7());
    fixture
        .prepare_with(&retryable_id, "sha256:payload-retryable")
        .await
        .unwrap();
    let accepted_id = HarnessInvocationId::new(Uuid::now_v7());
    let second_command = fixture.additional_command().await;
    let accepted = fixture
        .prepare_for(&second_command, &accepted_id, "sha256:payload-accepted")
        .await
        .unwrap();
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

    let others = fixture
        .exclude_other_invocations(&[retryable_id.clone(), accepted_id.clone()])
        .await;
    let first = fixture
        .store
        .recover_harness_invocations_excluding(1, &others)
        .await
        .unwrap();
    assert_eq!(first.len(), 1, "limit is a total row bound");
    assert_eq!(first[0].invocation_id, accepted_id);
    assert_eq!(first[0].state, HarnessInvocationState::Accepted);

    let mut after_first = others.clone();
    after_first.push(accepted_id.clone());
    let second = fixture
        .store
        .recover_harness_invocations_excluding(1, &after_first)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].invocation_id, retryable_id);
    assert_eq!(second[0].state, HarnessInvocationState::Prepared);

    let mut after_second = after_first;
    after_second.push(retryable_id.clone());
    let drained = fixture
        .store
        .recover_harness_invocations_excluding(1, &after_second)
        .await
        .unwrap();
    assert!(drained.is_empty());
}

#[tokio::test]
async fn cancelled_command_atomically_settles_pre_accept_and_late_terminals_as_cancelled() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let prepared = fixture.prepare("sha256:payload-cancelled").await.unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&prepared.invocation_id, 0)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 WHERE id = $1",
    )
    .bind(fixture.command_id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let retry = fixture
        .store
        .record_harness_retryable_failure(
            &prepared.invocation_id,
            0,
            3,
            "sha256:retry-after-cancel",
            &json!({"type":"failed","code":"harness_failed","detail":"late retry"}),
        )
        .await
        .unwrap();
    assert_eq!(retry.state, HarnessInvocationState::Cancelled);
    assert_eq!(retry.attempt, 0);
    assert_eq!(retry.terminal_payload, Some(json!({"type":"cancelled"})));

    let late = fixture
        .store
        .settle_harness_invocation(
            &prepared.invocation_id,
            0,
            HarnessInvocationState::Completed,
            "sha256:late-completion",
            &json!({"type":"completed","outcome":"ok","fields":{},"history":[]}),
        )
        .await
        .unwrap();
    assert_eq!(late.state, HarnessInvocationState::Cancelled);
    assert_eq!(late.terminal_payload, Some(json!({"type":"cancelled"})));
}

#[tokio::test]
async fn command_cancellation_lock_winner_forces_a_waiting_late_success_to_cancelled() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-cancel-race").await.unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation.invocation_id, 0)
        .await
        .unwrap();

    let mut cancellation = fixture.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM agent_commands WHERE id = $1 FOR UPDATE")
        .bind(fixture.command_id.as_uuid())
        .execute(&mut *cancellation)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 WHERE id = $1",
    )
    .bind(fixture.command_id.as_uuid())
    .execute(&mut *cancellation)
    .await
    .unwrap();

    let store = fixture.store.clone();
    let invocation_id = invocation.invocation_id.clone();
    let late_success = tokio::spawn(async move {
        store
            .settle_harness_invocation(
                &invocation_id,
                0,
                HarnessInvocationState::Completed,
                "sha256:late-success-race",
                &json!({"type":"completed","outcome":"ok","fields":{},"history":[]}),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !late_success.is_finished(),
        "terminal settlement waits for the authoritative command lock"
    );
    cancellation.commit().await.unwrap();

    let settled = late_success.await.unwrap().unwrap();
    assert_eq!(settled.state, HarnessInvocationState::Cancelled);
    assert_eq!(settled.attempt, 0);
    assert_eq!(settled.terminal_payload, Some(json!({"type":"cancelled"})));
}

#[tokio::test]
async fn completed_journal_requires_the_completed_command_cas_winner() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture
        .prepare("sha256:payload-completion-order")
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation.invocation_id, 0)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation.invocation_id, 0)
        .await
        .unwrap();

    let error = fixture
        .store
        .settle_harness_invocation(
            &invocation.invocation_id,
            0,
            HarnessInvocationState::Completed,
            "sha256:completion-before-command-cas",
            &json!({"type":"completed","outcome":"ok","fields":{},"history":[]}),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::RevisionConflict));
    let still_accepted = fixture
        .store
        .harness_invocation_for_command_revision(&fixture.command_id, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still_accepted.state, HarnessInvocationState::Accepted);

    sqlx::query(
        "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 WHERE id = $1",
    )
    .bind(fixture.command_id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let cancelled = fixture
        .store
        .settle_harness_invocation(
            &invocation.invocation_id,
            0,
            HarnessInvocationState::Completed,
            "sha256:late-completion",
            &json!({"type":"completed","outcome":"ok","fields":{},"history":[]}),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state, HarnessInvocationState::Cancelled);
}

#[tokio::test]
async fn recovery_settles_cancelled_command_rows_instead_of_reselecting_them_as_retryable() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture
        .prepare("sha256:payload-cancelled-recovery")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 WHERE id = $1",
    )
    .bind(fixture.command_id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();

    let excluded = fixture
        .exclude_other_invocations(std::slice::from_ref(&invocation.invocation_id))
        .await;
    let recovered = fixture
        .store
        .recover_harness_invocations_excluding(1, &excluded)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].invocation_id, invocation.invocation_id);
    assert_eq!(recovered[0].state, HarnessInvocationState::Cancelled);
    assert_eq!(
        recovered[0].terminal_payload,
        Some(json!({"type":"cancelled"}))
    );
}

#[tokio::test]
async fn two_command_recovery_uses_single_command_transactions_without_deadlock() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let first = fixture.prepare("sha256:first-lock-order").await.unwrap();
    let second_command = fixture.additional_command().await;
    let second = fixture
        .prepare_for(
            &second_command,
            &HarnessInvocationId::new(Uuid::now_v7()),
            "sha256:second-lock-order",
        )
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&second.invocation_id, 0)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&second.invocation_id, 0)
        .await
        .unwrap();

    let store_a = fixture.store.clone();
    let store_b = fixture.store.clone();
    let first_id = first.invocation_id.clone();
    let second_id = second.invocation_id.clone();
    let others = fixture
        .exclude_other_invocations(&[first_id.clone(), second_id.clone()])
        .await;
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(5), async move {
        let mut exclude_second = others.clone();
        exclude_second.push(second_id);
        let mut exclude_first = others;
        exclude_first.push(first_id);
        tokio::join!(
            store_a.recover_harness_invocations_excluding(1, &exclude_second),
            store_b.recover_harness_invocations_excluding(1, &exclude_first),
        )
    })
    .await
    .expect("two-command recovery must not deadlock");
    assert_eq!(a.unwrap().len(), 1);
    assert_eq!(b.unwrap().len(), 1);
}

#[tokio::test]
async fn stale_recovery_batch_refresh_converges_a_cancelled_sibling_before_counting() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let accepted = fixture.prepare("sha256:accepted-sibling").await.unwrap();
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
    let sibling_command = fixture.additional_command().await;
    let sibling = fixture
        .prepare_for(
            &sibling_command,
            &HarnessInvocationId::new(Uuid::now_v7()),
            "sha256:prepared-sibling",
        )
        .await
        .unwrap();
    let excluded = fixture
        .exclude_other_invocations(&[
            accepted.invocation_id.clone(),
            sibling.invocation_id.clone(),
        ])
        .await;
    let selected = fixture
        .store
        .recover_harness_invocations_excluding(2, &excluded)
        .await
        .unwrap();
    assert_eq!(selected.len(), 2);

    sqlx::query(
        "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 \
         WHERE id = $1 AND state = 'claimed'",
    )
    .bind(sibling_command.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let refreshed = fixture
        .store
        .refresh_harness_recovery_invocation(&sibling.invocation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.state, HarnessInvocationState::Cancelled);
    assert!(refreshed.terminal);
}

#[tokio::test]
async fn event_sequence_replay_is_idempotent_and_digest_drift_conflicts() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let event = fixture.event(
        &invocation.invocation_id,
        0,
        HarnessInvocationEvent::Started,
    );
    fixture
        .store
        .append_harness_invocation_event(&event)
        .await
        .unwrap();
    fixture
        .store
        .append_harness_invocation_event(&event)
        .await
        .unwrap();
    let progress = fixture.event(
        &invocation.invocation_id,
        1,
        HarnessInvocationEvent::Progress {
            message: "first bytes".to_owned(),
        },
    );
    fixture
        .store
        .append_harness_invocation_event(&progress)
        .await
        .unwrap();
    fixture
        .store
        .append_harness_invocation_event(&progress)
        .await
        .unwrap();
    let drift = fixture.event(
        &invocation.invocation_id,
        1,
        HarnessInvocationEvent::Progress {
            message: "different bytes".to_owned(),
        },
    );
    let error = fixture
        .store
        .append_harness_invocation_event(&drift)
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::IdempotencyConflict),
        "unexpected drift error: {error:?}"
    );
}

#[tokio::test]
async fn event_sequence_gap_after_started_is_rejected_while_exact_replay_succeeds() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let started = fixture.event(
        &invocation.invocation_id,
        0,
        HarnessInvocationEvent::Started,
    );
    fixture
        .store
        .append_harness_invocation_event(&started)
        .await
        .unwrap();
    fixture
        .store
        .append_harness_invocation_event(&started)
        .await
        .expect("exact replay remains idempotent");
    let gap = fixture.event(
        &invocation.invocation_id,
        2,
        HarnessInvocationEvent::Progress {
            message: "skipped sequence one".to_owned(),
        },
    );
    assert!(matches!(
        fixture
            .store
            .append_harness_invocation_event(&gap)
            .await
            .unwrap_err(),
        StoreError::RevisionConflict
    ));
}

#[tokio::test]
async fn event_only_digest_is_rejected_by_the_exported_store_boundary() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let mut record = fixture.event(
        &invocation.invocation_id,
        0,
        HarnessInvocationEvent::Started,
    );
    record.event_digest = kiteframe::canonical_sha256(&record.event).unwrap();

    assert!(matches!(
        fixture
            .store
            .append_harness_invocation_event(&record)
            .await
            .unwrap_err(),
        StoreError::InvalidData(_)
    ));
}

#[tokio::test]
async fn database_digest_matches_rust_v1_envelopes_for_every_event_shape() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let events = [
        HarnessInvocationEvent::Started,
        HarnessInvocationEvent::Progress {
            message: "working".to_owned(),
        },
        HarnessInvocationEvent::ApprovalRequested {
            request_id: "permission-7".to_owned(),
            title: "Need permission".to_owned(),
        },
        HarnessInvocationEvent::ApprovalResolved {
            request_id: "permission-7".to_owned(),
            decision: HarnessApprovalDecision::AllowOnce,
        },
        HarnessInvocationEvent::ApprovalResolved {
            request_id: "permission-8".to_owned(),
            decision: HarnessApprovalDecision::RejectOnce,
        },
        HarnessInvocationEvent::AssistantMessage {
            text: "committed".to_owned(),
        },
        HarnessInvocationEvent::Cancelled,
    ];
    for (sequence, event) in events.into_iter().enumerate() {
        let envelope = HarnessEventEnvelope::new(
            invocation.invocation_id.clone(),
            sequence as u64,
            event.clone(),
        );
        let database_digest: String =
            sqlx::query_scalar("SELECT harness_event_envelope_digest_v1($1, $2, $3)")
                .bind(invocation.invocation_id.as_uuid())
                .bind(sequence as i64)
                .bind(serde_json::to_value(event).unwrap())
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
        assert_eq!(database_digest, envelope.event_digest);
    }
}

#[tokio::test]
async fn database_rejects_sequence_zero_that_is_not_started() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let invalid = HarnessEventEnvelope::new(
        invocation.invocation_id.clone(),
        0,
        HarnessInvocationEvent::ApprovalRequested {
            request_id: "permission-7".to_owned(),
            title: "Need permission".to_owned(),
        },
    );
    let error = sqlx::query(
        "INSERT INTO harness_invocation_events (invocation_id, sequence, event_digest, event) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(invocation.invocation_id.as_uuid())
    .bind(0_i64)
    .bind(&invalid.event_digest)
    .bind(serde_json::to_value(&invalid.event).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55000")
    );
}

#[tokio::test]
async fn database_on_conflict_accepts_only_an_exact_event_replay() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let started = fixture.event(
        &invocation.invocation_id,
        0,
        HarnessInvocationEvent::Started,
    );
    fixture
        .store
        .append_harness_invocation_event(&started)
        .await
        .unwrap();

    let insert = "INSERT INTO harness_invocation_events \
        (invocation_id, sequence, event_digest, event) VALUES ($1, $2, $3, $4) \
        ON CONFLICT (invocation_id, sequence) DO NOTHING";
    sqlx::query(insert)
        .bind(invocation.invocation_id.as_uuid())
        .bind(0_i64)
        .bind(&started.event_digest)
        .bind(serde_json::to_value(&started.event).unwrap())
        .execute(&fixture.pool)
        .await
        .expect("an exact event replay remains idempotent at the database boundary");

    let error = sqlx::query(insert)
        .bind(invocation.invocation_id.as_uuid())
        .bind(0_i64)
        .bind("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
        .bind(serde_json::to_value(&started.event).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55000")
    );
}

#[tokio::test]
async fn approval_request_and_one_shot_decision_are_durable_events() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    fixture
        .append_approval_pair(
            &invocation.invocation_id,
            "permission-7",
            HarnessApprovalDecision::RejectOnce,
        )
        .await
        .unwrap();
    let events = fixture
        .store
        .harness_invocation_events(&invocation.invocation_id)
        .await
        .unwrap();
    assert!(matches!(
        events[1].event,
        HarnessInvocationEvent::ApprovalRequested { .. }
    ));
    assert!(matches!(
        events[2].event,
        HarnessInvocationEvent::ApprovalResolved {
            decision: HarnessApprovalDecision::RejectOnce,
            ..
        }
    ));
}

#[tokio::test]
async fn exhausted_retry_waits_for_the_command_authority_before_terminal_journaling() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation.invocation_id, 0)
        .await
        .unwrap();
    let retry = fixture
        .store
        .record_harness_retryable_failure(
            &invocation.invocation_id,
            0,
            1,
            "sha256:retry-a",
            &json!({"type":"failed","code":"harness_failed","detail":"retry"}),
        )
        .await
        .unwrap();
    assert_eq!(retry.state, HarnessInvocationState::Started);
    assert_eq!(retry.attempt, 1);
    let exhausted = fixture
        .store
        .record_harness_retryable_failure(
            &invocation.invocation_id,
            1,
            1,
            "sha256:retry-b",
            &json!({"type":"failed","code":"harness_failed","detail":"retry"}),
        )
        .await
        .unwrap();
    assert_eq!(exhausted.state, HarnessInvocationState::Started);
    assert_eq!(exhausted.attempt, 1);
    assert!(!exhausted.terminal);
    assert_eq!(exhausted.terminal_request_digest, None);
    assert_eq!(exhausted.terminal_payload, None);
}

#[tokio::test]
async fn registration_replay_and_terminal_replay_require_the_same_digests() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let registration = HarnessPluginRegistration {
        profile_id: fixture.profile_id.clone(),
        profile_digest: fixture.profile_digest.clone(),
        source_repository: "https://example.test/harness".to_owned(),
        source_revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        transport: HarnessTransport::AcpStdio,
        enabled: true,
    };
    fixture
        .store
        .register_harness_plugin(registration.clone())
        .await
        .unwrap();
    let mut drift = registration;
    drift.profile_digest = "sha256:profile-b".to_owned();
    assert!(matches!(
        fixture
            .store
            .register_harness_plugin(drift)
            .await
            .unwrap_err(),
        StoreError::IdempotencyConflict,
    ));

    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let terminal_key = format!("harness-terminal:{}", invocation.invocation_id.as_uuid());
    assert!(matches!(
        fixture
            .store
            .settle_harness_invocation(
                &invocation.invocation_id,
                0,
                HarnessInvocationState::Cancelled,
                &terminal_key,
                &json!({"type":"cancelled"}),
            )
            .await
            .unwrap_err(),
        StoreError::RevisionConflict,
    ));
    sqlx::query(
        "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1 WHERE id = $1",
    )
    .bind(fixture.command_id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();
    fixture
        .store
        .settle_harness_invocation(
            &invocation.invocation_id,
            0,
            HarnessInvocationState::Cancelled,
            &terminal_key,
            &json!({"type":"cancelled"}),
        )
        .await
        .unwrap();
    fixture
        .store
        .settle_harness_invocation(
            &invocation.invocation_id,
            0,
            HarnessInvocationState::Cancelled,
            &terminal_key,
            &json!({"type":"cancelled"}),
        )
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .store
            .settle_harness_invocation(
                &invocation.invocation_id,
                0,
                HarnessInvocationState::Cancelled,
                "sha256:terminal-b",
                &json!({"type":"cancelled"}),
            )
            .await
            .unwrap_err(),
        StoreError::IdempotencyConflict,
    ));
}

#[tokio::test]
async fn completed_command_receipt_must_match_the_exact_harness_terminal() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:receipt-identity").await.unwrap();
    fixture
        .store
        .mark_harness_invocation_started(&invocation.invocation_id, 0)
        .await
        .unwrap();
    fixture
        .store
        .mark_harness_prompt_accepted(&invocation.invocation_id, 0)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE agent_commands SET state = 'completed', revision = revision + 1, \
         completion_idempotency_key = 'harness-result:wrong', \
         completion_request_digest = 'sha256:wrong', \
         result = $1 WHERE id = $2",
    )
    .bind(json!({
        "type":"command_failed",
        "payload":{
            "event_id":"command-failed:wrong",
            "command_key":"harness:store",
            "code":"harness_failed"
        }
    }))
    .bind(fixture.command_id.as_uuid())
    .execute(&fixture.pool)
    .await
    .unwrap();

    assert!(matches!(
        fixture
            .store
            .settle_harness_invocation(
                &invocation.invocation_id,
                0,
                HarnessInvocationState::Completed,
                &format!("harness-terminal:{}", invocation.invocation_id.as_uuid()),
                &json!({"type":"completed","outcome":"ok","fields":{},"history":[]}),
            )
            .await
            .unwrap_err(),
        StoreError::IdempotencyConflict,
    ));
}

#[tokio::test]
async fn immutable_identity_and_invalid_state_edges_are_rejected_by_database() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let identity_error = sqlx::query("UPDATE harness_invocations SET payload_digest = 'sha256:tampered' WHERE invocation_id = $1")
        .bind(invocation.invocation_id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap_err();
    assert_eq!(
        identity_error
            .as_database_error()
            .unwrap()
            .code()
            .as_deref(),
        Some("55000")
    );
    let edge_error =
        sqlx::query("UPDATE harness_invocations SET state = 'accepted' WHERE invocation_id = $1")
            .bind(invocation.invocation_id.as_uuid())
            .execute(&fixture.pool)
            .await
            .unwrap_err();
    assert_eq!(
        edge_error.as_database_error().unwrap().code().as_deref(),
        Some("55000")
    );
    let delete_error = sqlx::query("DELETE FROM harness_invocations WHERE invocation_id = $1")
        .bind(invocation.invocation_id.as_uuid())
        .execute(&fixture.pool)
        .await
        .unwrap_err();
    assert_eq!(
        delete_error.as_database_error().unwrap().code().as_deref(),
        Some("55000")
    );
}
