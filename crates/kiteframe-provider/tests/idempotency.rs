use std::sync::atomic::{AtomicUsize, Ordering};

use kiteframe_contract::{
    ActorRef, AdmissionId, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CatalogIdentity, IdempotencyKey, InvocationId, NormalizedResourceSelector,
    ProtectedEvidenceRequestRef, Sha256Digest, StatusRequest, Timestamp, TraceContext,
};
use kiteframe_provider::{
    AbandonmentAuthorization, IdempotencyScopeValue, InMemoryInvocationStore, InvocationState,
    InvocationStatusContext, InvocationStore, ReservationKind, StatusState, StoredInvocation,
};

#[tokio::test]
async fn same_scope_and_key_deduplicates_effect() {
    let store = InMemoryInvocationStore::new();
    let effect_count = AtomicUsize::new(0);

    let first =
        execute_if_reserved(&store, reservation("inv-1", "key-1", 100), &effect_count).await;
    let second =
        execute_if_reserved(&store, reservation("inv-2", "key-1", 101), &effect_count).await;

    assert_eq!(first.invocation_id().as_str(), "inv-1");
    assert_eq!(second.invocation_id().as_str(), "inv-1");
    assert_eq!(effect_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_durable_reservation_is_publicly_pending() {
    let store = InMemoryInvocationStore::new();

    let reservation = store
        .reserve_or_get(reservation("inv-1", "key-1", 100))
        .await
        .unwrap();

    assert_eq!(reservation.status().status_state(), StatusState::Pending);
}

#[tokio::test]
async fn same_key_with_different_request_is_rejected() {
    let store = InMemoryInvocationStore::new();
    store
        .reserve_or_get(reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    let mut conflicting = reservation("inv-2", "key-1", 101);
    conflicting.request_digest = digest(99);

    let error = store.reserve_or_get(conflicting).await.unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-002");
}

#[tokio::test]
async fn duplicate_reservation_does_not_bypass_status_context_authorization() {
    let store = InMemoryInvocationStore::new();
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
async fn unknown_outcome_rejects_new_key_until_resolved() {
    let store = InMemoryInvocationStore::new();
    let reserved = store
        .reserve_or_get(reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    store
        .transition(
            reserved.status().invocation_id(),
            InvocationState::Reserved,
            InvocationState::OutcomeUnknown,
        )
        .await
        .unwrap();

    let error = store
        .reserve_or_get(reservation("inv-2", "key-2", 101))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-003");
    assert_eq!(error.retry, kiteframe_contract::RetryClass::StatusFirst);
}

#[tokio::test]
async fn unknown_outcome_remains_blocking_after_its_retention_deadline() {
    let store = InMemoryInvocationStore::new();
    let mut first = reservation("inv-1", "key-1", 1);
    first.retention_until = Timestamp::new(2);
    store.reserve_or_get(first).await.unwrap();
    store
        .transition(
            &InvocationId::new("inv-1").unwrap(),
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
async fn exact_status_context_is_required() {
    let store = InMemoryInvocationStore::new();
    store
        .reserve_or_get(reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    let request = StatusRequest::new(InvocationId::new("inv-1").unwrap(), trace_context("1"));

    assert!(
        store
            .status(&request, &status_context("human-7"))
            .await
            .is_ok()
    );
    let error = store
        .status(&request, &status_context("human-8"))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-003");
}

#[tokio::test]
async fn transitions_compare_and_swap_and_abandonment_is_explicit() {
    let store = InMemoryInvocationStore::new();
    store
        .reserve_or_get(reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    let invocation_id = InvocationId::new("inv-1").unwrap();
    store
        .transition(
            &invocation_id,
            InvocationState::Reserved,
            InvocationState::OutcomeUnknown,
        )
        .await
        .unwrap();

    let stale = store
        .transition(
            &invocation_id,
            InvocationState::Reserved,
            InvocationState::Succeeded {
                result: serde_json::json!({"ok": true}),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code.as_str(), "KF-CAP-002");

    store
        .abandon(
            &invocation_id,
            &status_context("human-7"),
            AbandonmentAuthorization::try_new("audit-authz-9", "operator-3").unwrap(),
        )
        .await
        .unwrap();
    let replacement = store
        .reserve_or_get(reservation("inv-2", "key-2", 102))
        .await
        .unwrap();
    assert_eq!(replacement.kind(), ReservationKind::Reserved);
}

async fn execute_if_reserved(
    store: &InMemoryInvocationStore,
    input: StoredInvocation,
    effect_count: &AtomicUsize,
) -> kiteframe_provider::InvocationStatus {
    let reservation = store.reserve_or_get(input).await.unwrap();
    if reservation.kind() == ReservationKind::Reserved {
        effect_count.fetch_add(1, Ordering::SeqCst);
    }
    reservation.status().clone()
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
    InvocationStatusContext::try_new(
        "tenant-1",
        human,
        "workload-2",
        "run-9",
        "actor-7",
        "agent-2",
        "task-4",
        "session-3",
        "admission-5",
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
