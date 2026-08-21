use std::sync::Arc;

use kiteframe::{
    AgentCommandId, CreateSession, PostgresStore, PrincipalId, StoreError, canonical_sha256,
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
                HarnessInvocationEvent::ApprovalRequested {
                    request_id: request_id.to_owned(),
                    title: "Need permission".to_owned(),
                },
            ))
            .await?;
        self.store
            .append_harness_invocation_event(&self.event(
                invocation_id,
                1,
                HarnessInvocationEvent::ApprovalResolved {
                    request_id: request_id.to_owned(),
                    decision,
                },
            ))
            .await
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
        "terminal_payload IS NOT NULL",
        "terminal harness invocation is immutable",
    ] {
        assert!(
            sql.contains(required),
            "missing terminal replay guard: {required}"
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
        .unwrap_err();
    assert!(matches!(identity_drift, StoreError::IdempotencyConflict));
    let second_command = fixture.additional_command().await;
    let command_drift = fixture
        .prepare_for(&second_command, &invocation_id, "sha256:payload-a")
        .await
        .unwrap_err();
    assert!(matches!(command_drift, StoreError::IdempotencyConflict));
}

#[tokio::test]
async fn accepted_process_death_is_recovered_as_outcome_unknown() {
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
    let recovered = fixture.store.recover_harness_invocations(16).await.unwrap();
    assert!(recovered.iter().any(|view| {
        view.invocation_id == invocation.invocation_id
            && view.state == HarnessInvocationState::OutcomeUnknown
    }));
}

#[tokio::test]
async fn recovery_settles_all_accepted_rows_before_limiting_retryable_work() {
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

    let recovered = fixture.store.recover_harness_invocations(1).await.unwrap();
    assert!(recovered.iter().any(|view| {
        view.invocation_id == accepted_id && view.state == HarnessInvocationState::OutcomeUnknown
    }));
    assert!(recovered.iter().any(|view| {
        view.invocation_id == retryable_id && view.state == HarnessInvocationState::Prepared
    }));
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
    let drift = fixture.event(
        &invocation.invocation_id,
        0,
        HarnessInvocationEvent::Progress {
            message: "different bytes".to_owned(),
        },
    );
    assert!(matches!(
        fixture
            .store
            .append_harness_invocation_event(&drift)
            .await
            .unwrap_err(),
        StoreError::IdempotencyConflict,
    ));
}

#[tokio::test]
async fn legacy_event_digest_replays_a_versioned_envelope_when_content_is_identical() {
    let Some(fixture) = HarnessStoreFixture::new().await else {
        return;
    };
    let invocation = fixture.prepare("sha256:payload-a").await.unwrap();
    let envelope = HarnessEventEnvelope::new(
        invocation.invocation_id.clone(),
        0,
        HarnessInvocationEvent::Started,
    );
    let legacy_digest = canonical_sha256(&envelope.event).unwrap();
    sqlx::query(
        "INSERT INTO harness_invocation_events (invocation_id, sequence, event_digest, event) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(invocation.invocation_id.as_uuid())
    .bind(0_i64)
    .bind(&legacy_digest)
    .bind(serde_json::to_value(&envelope.event).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();

    fixture
        .store
        .append_harness_event_envelope(&envelope)
        .await
        .expect("identical legacy event replays successfully");
    let stored = fixture
        .store
        .harness_invocation_events(&invocation.invocation_id)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].event_digest, legacy_digest);
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
        events[0].event,
        HarnessInvocationEvent::ApprovalRequested { .. }
    ));
    assert!(matches!(
        events[1].event,
        HarnessInvocationEvent::ApprovalResolved {
            decision: HarnessApprovalDecision::RejectOnce,
            ..
        }
    ));
}

#[tokio::test]
async fn retryable_failure_retries_only_before_prompt_acceptance_and_then_fails() {
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
    let failed = fixture
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
    assert_eq!(failed.state, HarnessInvocationState::Failed);
    assert!(failed.terminal);
    fixture
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
    assert!(matches!(
        fixture
            .store
            .record_harness_retryable_failure(
                &invocation.invocation_id,
                1,
                1,
                "sha256:retry-c",
                &json!({"type":"failed","code":"harness_failed","detail":"retry"})
            )
            .await
            .unwrap_err(),
        StoreError::IdempotencyConflict,
    ));
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
    fixture
        .store
        .settle_harness_invocation(
            &invocation.invocation_id,
            0,
            HarnessInvocationState::Cancelled,
            "sha256:terminal-a",
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
            "sha256:terminal-a",
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
