use std::sync::Arc;

use kiteframe_contract::{
    ActorRef, AdmissionId, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CatalogIdentity, IdempotencyKey, InvocationId, NormalizedResourceSelector,
    ProtectedEvidenceRequestRef, RetryClass, Sha256Digest, StableCapabilityError, StatusRequest,
    Timestamp, TraceContext,
};
use kiteframe_provider::{
    AbandonmentAuthorization, IdempotencyScopeValue, InvocationState, InvocationStatusContext,
    InvocationStore, ReservationKind, StatusState, StoredInvocation,
};
use kiteframe_provider_sqlite::SqliteInvocationStore;
use sqlx::{Connection, Row};

#[tokio::test]
async fn status_survives_store_restart_with_digests_and_trace() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invocations.sqlite3");
    let store = SqliteInvocationStore::open(&path).await.unwrap();
    let input = reservation("inv-1", "key-1", 100);
    store.reserve_or_get(input.clone()).await.unwrap();
    store
        .transition(
            &input.invocation_id,
            InvocationState::Reserved,
            InvocationState::OutcomeUnknown,
        )
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteInvocationStore::open(&path).await.unwrap();
    let request = StatusRequest::new(input.invocation_id.clone(), trace_context("a"));
    let status = reopened
        .status(&request, &status_context("human-7"))
        .await
        .unwrap();

    assert_eq!(status.status_state(), StatusState::OutcomeUnknown);
    assert_eq!(status.request_digest(), &digest(1));
    assert_eq!(status.grant_digest(), &digest(2));
    assert_eq!(status.catalog_identity(), &input.catalog_identity);
    assert_eq!(status.catalog_digest(), &digest(3));
    assert_eq!(status.descriptor_digest(), &digest(4));
    assert_eq!(status.authority_revision_digest(), &digest(5));
    assert_eq!(status.proposal_digest(), &digest(6));
    assert_eq!(
        reopened.last_traceparent().as_deref(),
        Some("00-0123456789abcdef0123456789abcdef-aaaaaaaaaaaaaaaa-01")
    );
}

#[tokio::test]
async fn concurrent_duplicate_reservation_has_one_owner() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteInvocationStore::open(directory.path().join("concurrent.sqlite3"))
            .await
            .unwrap(),
    );
    let left_store = store.clone();
    let right_store = store.clone();
    let left = tokio::spawn(async move {
        left_store
            .reserve_or_get(reservation("inv-1", "key-1", 100))
            .await
            .unwrap()
    });
    let right = tokio::spawn(async move {
        right_store
            .reserve_or_get(reservation("inv-2", "key-1", 100))
            .await
            .unwrap()
    });

    let kinds = [left.await.unwrap().kind(), right.await.unwrap().kind()];
    assert_eq!(
        kinds
            .into_iter()
            .filter(|kind| *kind == ReservationKind::Reserved)
            .count(),
        1
    );
}

#[tokio::test]
async fn status_denies_any_principal_or_portable_context_mismatch() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteInvocationStore::open(directory.path().join("auth.sqlite3"))
        .await
        .unwrap();
    let input = reservation("inv-1", "key-1", 100);
    store.reserve_or_get(input.clone()).await.unwrap();
    let request = StatusRequest::new(input.invocation_id, trace_context("b"));

    let mismatches = [
        context([
            "tenant-2",
            "human-7",
            "workload-2",
            "run-9",
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-8",
            "workload-2",
            "run-9",
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-7",
            "workload-3",
            "run-9",
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-7",
            "workload-2",
            "run-8",
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-7",
            "workload-2",
            "run-9",
            "actor-8",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-7",
            "workload-2",
            "run-9",
            "actor-7",
            "agent-3",
            "task-4",
            "session-3",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-7",
            "workload-2",
            "run-9",
            "actor-7",
            "agent-2",
            "task-5",
            "session-3",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-7",
            "workload-2",
            "run-9",
            "actor-7",
            "agent-2",
            "task-4",
            "session-4",
            "admission-5",
        ]),
        context([
            "tenant-1",
            "human-7",
            "workload-2",
            "run-9",
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-6",
        ]),
    ];
    for mismatch in mismatches {
        let error = store.status(&request, &mismatch).await.unwrap_err();
        assert_eq!(error.code.as_str(), "KF-AUTH-003");
    }
}

#[tokio::test]
async fn duplicate_reservation_requires_the_original_exact_context() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteInvocationStore::open(directory.path().join("duplicate-auth.sqlite3"))
        .await
        .unwrap();
    store
        .reserve_or_get(reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    let mut other_principal = reservation("inv-2", "key-1", 101);
    other_principal.status_context = status_context("human-8");

    let error = store.reserve_or_get(other_principal).await.unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
}

#[tokio::test]
async fn expired_reservation_releases_the_exact_key() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteInvocationStore::open(directory.path().join("retention.sqlite3"))
        .await
        .unwrap();
    let mut expired = reservation("inv-1", "key-1", 1);
    expired.retention_until = Timestamp::new(2);
    store.reserve_or_get(expired).await.unwrap();

    let replacement = store
        .reserve_or_get(reservation("inv-2", "key-1", 3))
        .await
        .unwrap();

    assert_eq!(replacement.kind(), ReservationKind::Reserved);
    assert_eq!(replacement.status().invocation_id().as_str(), "inv-2");
}

#[tokio::test]
async fn unresolved_unknown_outcome_is_not_purged_at_retention_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteInvocationStore::open(directory.path().join("unknown-retention.sqlite3"))
        .await
        .unwrap();
    let mut first = reservation("inv-1", "key-1", 1);
    first.retention_until = Timestamp::new(2);
    store.reserve_or_get(first.clone()).await.unwrap();
    store
        .transition(
            &first.invocation_id,
            InvocationState::Reserved,
            InvocationState::OutcomeUnknown,
        )
        .await
        .unwrap();

    let error = store
        .reserve_or_get(reservation("inv-2", "key-2", 3))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-003");
}

#[tokio::test]
async fn unknown_outcome_blocks_new_key_across_restart_until_authorized_abandonment() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("abandon.sqlite3");
    let store = SqliteInvocationStore::open(&path).await.unwrap();
    let first = reservation("inv-1", "key-1", 100);
    store.reserve_or_get(first.clone()).await.unwrap();
    store
        .transition(
            &first.invocation_id,
            InvocationState::Reserved,
            InvocationState::OutcomeUnknown,
        )
        .await
        .unwrap();
    drop(store);
    let reopened = SqliteInvocationStore::open(&path).await.unwrap();

    let error = reopened
        .reserve_or_get(reservation("inv-2", "key-2", 101))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-003");

    reopened
        .abandon(
            &first.invocation_id,
            &status_context("human-7"),
            AbandonmentAuthorization::try_new("audit-authz-9", "operator-3").unwrap(),
        )
        .await
        .unwrap();
    let replacement = reopened
        .reserve_or_get(reservation("inv-2", "key-2", 102))
        .await
        .unwrap();
    assert_eq!(replacement.kind(), ReservationKind::Reserved);
    drop(reopened);

    let reopened = SqliteInvocationStore::open(&path).await.unwrap();
    let status = reopened
        .status(
            &StatusRequest::new(first.invocation_id, trace_context("c")),
            &status_context("human-7"),
        )
        .await
        .unwrap();
    assert_eq!(
        status.abandonment_authorization_record_id(),
        Some("audit-authz-9")
    );
    assert_eq!(status.abandoned_by(), Some("operator-3"));
}

#[tokio::test]
async fn safe_terminal_result_error_and_audit_refs_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("terminal.sqlite3");
    let store = SqliteInvocationStore::open(&path).await.unwrap();
    let mut success = reservation("inv-success", "key-success", 100);
    success.audit_authorization_record_id = Some("audit-authz-success".to_owned());
    success.audit_outcome_record_id = Some("audit-outcome-success".to_owned());
    store.reserve_or_get(success.clone()).await.unwrap();
    store
        .transition(
            &success.invocation_id,
            InvocationState::Reserved,
            InvocationState::Pending,
        )
        .await
        .unwrap();
    store
        .transition(
            &success.invocation_id,
            InvocationState::Pending,
            InvocationState::Succeeded {
                result: serde_json::json!({"caseId": "42"}),
            },
        )
        .await
        .unwrap();

    let mut failure = reservation("inv-failure", "key-failure", 101);
    failure.audit_authorization_record_id = Some("audit-authz-failure".to_owned());
    failure.audit_outcome_record_id = Some("audit-outcome-failure".to_owned());
    store.reserve_or_get(failure.clone()).await.unwrap();
    store
        .transition(
            &failure.invocation_id,
            InvocationState::Reserved,
            InvocationState::Pending,
        )
        .await
        .unwrap();
    let stable_error = StableCapabilityError::try_new(
        "CASE_CONFLICT",
        "conflict",
        RetryClass::AfterRefresh,
        "case changed",
    )
    .unwrap();
    store
        .transition(
            &failure.invocation_id,
            InvocationState::Pending,
            InvocationState::Failed {
                error: stable_error.clone(),
            },
        )
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteInvocationStore::open(&path).await.unwrap();
    let success_status = reopened
        .status(
            &StatusRequest::new(success.invocation_id, trace_context("d")),
            &status_context("human-7"),
        )
        .await
        .unwrap();
    assert_eq!(
        success_status.state(),
        &InvocationState::Succeeded {
            result: serde_json::json!({"caseId": "42"})
        }
    );
    assert_eq!(
        success_status.audit_authorization_record_id(),
        Some("audit-authz-success")
    );
    assert_eq!(
        success_status.audit_outcome_record_id(),
        Some("audit-outcome-success")
    );
    let failure_status = reopened
        .status(
            &StatusRequest::new(failure.invocation_id, trace_context("e")),
            &status_context("human-7"),
        )
        .await
        .unwrap();
    assert_eq!(
        failure_status.state(),
        &InvocationState::Failed {
            error: stable_error
        }
    );
}

#[tokio::test]
async fn migration_has_no_secret_evidence_or_provider_acl_columns() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema.sqlite3");
    let store = SqliteInvocationStore::open(&path).await.unwrap();
    drop(store);
    let url = format!("sqlite://{}", path.display());
    let mut connection = sqlx::SqliteConnection::connect(&url).await.unwrap();
    let columns = sqlx::query("PRAGMA table_info(invocations)")
        .fetch_all(&mut connection)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<Vec<_>>();

    for forbidden in [
        "credential",
        "token",
        "cookie",
        "claims",
        "evidence_body",
        "provider_acl",
        "legacy",
    ] {
        assert!(
            columns.iter().all(|column| !column.contains(forbidden)),
            "forbidden durable column: {forbidden}"
        );
    }
    for required in [
        "request_digest",
        "grant_digest",
        "catalog_digest",
        "descriptor_digest",
        "authority_revision_digest",
        "protected_evidence_refs_json",
        "audit_authorization_record_id",
        "audit_outcome_record_id",
        "retention_until",
    ] {
        assert!(columns.iter().any(|column| column == required));
    }
}

fn reservation(invocation_id: &str, key: &str, now: u64) -> StoredInvocation {
    StoredInvocation {
        invocation_id: InvocationId::new(invocation_id).unwrap(),
        status_id: format!("status-{invocation_id}"),
        scope: IdempotencyScopeValue::try_new(
            ActorRef::new("actor-7").unwrap(),
            capability(),
            NormalizedResourceSelector::new("case:42").unwrap(),
            "cases.update",
        )
        .unwrap(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        request_digest: digest(1),
        admission_id: AdmissionId::new("admission-5").unwrap(),
        grant_digest: digest(2),
        catalog_identity: CatalogIdentity {
            name: "provider.catalog".to_owned(),
            revision: "1.0.0".to_owned(),
        },
        catalog_digest: digest(3),
        descriptor_digest: digest(4),
        authority_revision_digest: digest(5),
        status_context: status_context("human-7"),
        proposal_digest: digest(6),
        protected_evidence_refs: vec![
            ProtectedEvidenceRequestRef::new("evidence://approval-1").unwrap(),
        ],
        state: InvocationState::Reserved,
        audit_authorization_record_id: None,
        audit_outcome_record_id: None,
        created_at: Timestamp::new(now),
        updated_at: Timestamp::new(now),
        retention_until: Timestamp::new(now + 3600),
        abandonment: None,
    }
}

fn status_context(human: &str) -> InvocationStatusContext {
    context([
        "tenant-1",
        human,
        "workload-2",
        "run-9",
        "actor-7",
        "agent-2",
        "task-4",
        "session-3",
        "admission-5",
    ])
}

fn context(values: [&str; 9]) -> InvocationStatusContext {
    InvocationStatusContext::try_new(
        values[0], values[1], values[2], values[3], values[4], values[5], values[6], values[7],
        values[8],
    )
    .unwrap()
}

fn capability() -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.update").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}

fn trace_context(parent_nibble: &str) -> TraceContext {
    TraceContext::try_new(
        format!(
            "00-0123456789abcdef0123456789abcdef-{}-01",
            parent_nibble.repeat(16)
        ),
        None,
        Default::default(),
    )
    .unwrap()
}
