use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use kiteframe_contract::{
    ActorRef, AdmissionId, ApprovalRequirement, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityIdentity, CapabilityName, CapabilityReleaseVersion, CatalogIdentity, CheckpointRef,
    ConfirmationRequirement, ConsentRequirement, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, EffectClassification, EvidenceKind, ExecutionMode, FreshnessRequirement,
    IdempotencyKey, IdempotencyRequirement, IdempotencyScope, InvocationId, NonEmptySet,
    NormalizedResourceSelector, ProtectedEvidenceRequestRef, ResourceSelectorSchema, Sha256Digest,
    StatusRequest, Suspension, Timestamp, TraceContext,
};
use kiteframe_provider::{
    AbandonmentAuthorization, IdempotencyScopeValue, InMemoryInvocationStore,
    InvocationAuditLinkKind, InvocationReservationInput, InvocationState, InvocationStatusContext,
    InvocationStore, InvocationStoreClock, InvocationTransition, ReservationKind, StatusSafeError,
    StatusSafeResult, StatusState, TransitionAuditRecord,
};
use serde_json::json;

#[test]
fn status_safe_result_projects_only_provider_owned_fields() {
    let descriptor = descriptor(json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["apiKey", "authorizationHeader", "note", "changed"],
        "properties": {
            "apiKey": {"type": "string"},
            "authorizationHeader": {"type": "string"},
            "note": {"type": "string"},
            "changed": {"type": "boolean"}
        }
    }));
    let result = StatusSafeResult::try_new(
        json!({
            "apiKey": "key-without-an-existing-marker",
            "authorizationHeader": "signed opaque material",
            "note": "sk_live_not-covered-by-the-old-denylist",
            "changed": true
        }),
        &descriptor,
    )
    .unwrap();

    assert_eq!(result.value(), &json!({"changed": true}));
}

#[test]
fn status_safe_error_never_projects_arbitrary_diagnostic_or_stable_messages() {
    let mut diagnostic = Diagnostic::error(
        DiagnosticCode::InvocationDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        "sk_live_not-covered-by-the-old-denylist",
    );
    diagnostic.details.insert(
        "authorizationHeader".to_owned(),
        json!("signed opaque material"),
    );
    let projected_diagnostic = StatusSafeError::try_from_diagnostic(&diagnostic).unwrap();
    assert_eq!(projected_diagnostic.message(), "invocation was denied");
    let diagnostic_wire = serde_json::to_string(&projected_diagnostic).unwrap();
    assert!(!diagnostic_wire.contains("sk_live"));
    assert!(!diagnostic_wire.contains("signed opaque material"));

    let stable = kiteframe_contract::StableCapabilityError::try_new(
        "CASE_CONFLICT",
        "conflict",
        kiteframe_contract::RetryClass::AfterRefresh,
        "authorizationHeader: signed opaque material",
    )
    .unwrap();
    let projected_stable = StatusSafeError::try_from_stable(&stable).unwrap();
    assert_eq!(projected_stable.message(), "capability invocation failed");
    assert!(
        !serde_json::to_string(&projected_stable)
            .unwrap()
            .contains("signed opaque material")
    );
}

#[tokio::test]
async fn descriptor_retention_window_is_enforced_from_trusted_store_time() {
    let clock = Arc::new(FakeClock(AtomicU64::new(100)));
    let store = InMemoryInvocationStore::with_clock(clock.clone());
    let descriptor = descriptor(standard_output_schema());

    let short = store
        .reserve_or_get(
            reservation("inv-1", "key-1", 1),
            &descriptor,
            Timestamp::new(3_699),
        )
        .await
        .unwrap_err();
    assert_eq!(short.code.as_str(), "KF-CAP-002");

    clock.0.store(5_000, Ordering::SeqCst);
    let stale_future_deadline = store
        .reserve_or_get(
            reservation("inv-2", "key-2", u64::MAX - 3_600),
            &descriptor,
            Timestamp::new(6_000),
        )
        .await
        .unwrap_err();
    assert_eq!(stale_future_deadline.code.as_str(), "KF-CAP-002");
}

#[tokio::test]
async fn audit_record_ids_attach_atomically_with_corresponding_transitions() {
    let clock = Arc::new(FakeClock(AtomicU64::new(100)));
    let store = InMemoryInvocationStore::with_clock(clock.clone());
    let descriptor = descriptor(standard_output_schema());
    let reserved = store
        .reserve_or_get(
            reservation("inv-1", "key-1", 99_999),
            &descriptor,
            Timestamp::new(3_700),
        )
        .await
        .unwrap();
    assert_eq!(
        reserved.status().audit_authorization_record_id(),
        None,
        "write-ahead authorization does not exist at reservation"
    );
    assert_eq!(reserved.status().audit_outcome_record_id(), None);

    clock.0.store(101, Ordering::SeqCst);
    store
        .transition(
            reserved.status().invocation_id(),
            InvocationTransition::try_new(
                InvocationState::Reserved,
                InvocationState::Pending,
                TransitionAuditRecord::Authorization("audit-authz-1".to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let pending = store
        .status(
            &StatusRequest::new(
                reserved.status().invocation_id().clone(),
                trace_context("2"),
            ),
            &status_context("human-7"),
        )
        .await
        .unwrap();
    assert_eq!(
        pending.audit_authorization_record_id(),
        Some("audit-authz-1")
    );
    assert_eq!(pending.audit_outcome_record_id(), None);

    clock.0.store(102, Ordering::SeqCst);
    let result = StatusSafeResult::try_new(json!({"caseId": "42"}), &descriptor).unwrap();
    store
        .transition(
            reserved.status().invocation_id(),
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::Succeeded { result },
                TransitionAuditRecord::Outcome("audit-outcome-1".to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let succeeded = store
        .status(
            &StatusRequest::new(
                reserved.status().invocation_id().clone(),
                trace_context("3"),
            ),
            &status_context("human-7"),
        )
        .await
        .unwrap();
    assert_eq!(
        succeeded.audit_authorization_record_id(),
        Some("audit-authz-1")
    );
    assert_eq!(succeeded.audit_outcome_record_id(), Some("audit-outcome-1"));
}

#[tokio::test]
async fn audit_history_appends_every_receipt_across_resume_and_unknown_resolution() {
    let clock = Arc::new(FakeClock(AtomicU64::new(100)));
    let store = InMemoryInvocationStore::with_clock(clock.clone());
    let descriptor = descriptor(standard_output_schema());
    let input = reservation("inv-history", "key-history", 0);
    store
        .reserve_or_get(input.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();

    let transitions = [
        (
            InvocationState::Reserved,
            InvocationState::Pending,
            TransitionAuditRecord::Authorization("audit-authz-initial".to_owned()),
        ),
        (
            InvocationState::Pending,
            InvocationState::Suspended {
                suspension: Box::new(test_suspension()),
            },
            TransitionAuditRecord::None,
        ),
        (
            InvocationState::Suspended {
                suspension: Box::new(test_suspension()),
            },
            InvocationState::Pending,
            TransitionAuditRecord::Authorization("audit-authz-resumed".to_owned()),
        ),
        (
            InvocationState::Pending,
            InvocationState::OutcomeUnknown,
            TransitionAuditRecord::Outcome("audit-outcome-unknown".to_owned()),
        ),
        (
            InvocationState::OutcomeUnknown,
            InvocationState::Succeeded {
                result: StatusSafeResult::try_new(json!({"caseId": "42"}), &descriptor).unwrap(),
            },
            TransitionAuditRecord::Outcome("audit-outcome-final".to_owned()),
        ),
    ];
    for (offset, (expected, next, audit)) in transitions.into_iter().enumerate() {
        clock.0.store(101 + offset as u64, Ordering::SeqCst);
        store
            .transition(
                &input.invocation_id,
                InvocationTransition::try_new(expected, next, audit).unwrap(),
            )
            .await
            .unwrap();
    }

    let status = store
        .status(
            &StatusRequest::new(input.invocation_id, trace_context("4")),
            &status_context("human-7"),
        )
        .await
        .unwrap();
    let links = status
        .audit_links()
        .iter()
        .map(|link| (link.kind(), link.record_id()))
        .collect::<Vec<_>>();
    assert_eq!(
        links,
        vec![
            (
                InvocationAuditLinkKind::Authorization,
                "audit-authz-initial"
            ),
            (
                InvocationAuditLinkKind::Authorization,
                "audit-authz-resumed"
            ),
            (InvocationAuditLinkKind::Outcome, "audit-outcome-unknown"),
            (InvocationAuditLinkKind::Outcome, "audit-outcome-final"),
        ]
    );
}

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

    let reservation = reserve_default(&store, reservation("inv-1", "key-1", 100))
        .await
        .unwrap();

    assert_eq!(reservation.status().status_state(), StatusState::Pending);
}

#[tokio::test]
async fn same_key_with_different_request_is_rejected() {
    let store = InMemoryInvocationStore::new();
    reserve_default(&store, reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    let mut conflicting = reservation("inv-2", "key-1", 101);
    conflicting.request_digest = digest(99);

    let error = reserve_default(&store, conflicting).await.unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-002");
}

#[tokio::test]
async fn duplicate_reservation_does_not_bypass_status_context_authorization() {
    let store = InMemoryInvocationStore::new();
    reserve_default(&store, reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    let mut other_principal = reservation("inv-2", "key-1", 101);
    other_principal.status_context = status_context("human-8");

    let error = reserve_default(&store, other_principal).await.unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
}

#[tokio::test]
async fn unknown_outcome_rejects_new_key_until_resolved() {
    let store = InMemoryInvocationStore::new();
    let reserved = reserve_default(&store, reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    mark_unknown(&store, reserved.status().invocation_id()).await;

    let error = reserve_default(&store, reservation("inv-2", "key-2", 101))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-003");
    assert_eq!(error.retry, kiteframe_contract::RetryClass::StatusFirst);
}

#[tokio::test]
async fn unknown_outcome_remains_blocking_after_its_retention_deadline() {
    let clock = Arc::new(FakeClock(AtomicU64::new(1)));
    let store = InMemoryInvocationStore::with_clock(clock.clone());
    let descriptor = descriptor_with_retention(standard_output_schema(), 1);
    store
        .reserve_or_get(
            reservation("inv-1", "key-1", 1),
            &descriptor,
            Timestamp::new(2),
        )
        .await
        .unwrap();
    mark_unknown(&store, &InvocationId::new("inv-1").unwrap()).await;

    clock.0.store(3, Ordering::SeqCst);
    let error = store
        .reserve_or_get(
            reservation("inv-2", "key-2", 3),
            &descriptor,
            Timestamp::new(4),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-003");
}

#[tokio::test]
async fn exact_status_context_is_required() {
    let store = InMemoryInvocationStore::new();
    reserve_default(&store, reservation("inv-1", "key-1", 100))
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
    reserve_default(&store, reservation("inv-1", "key-1", 100))
        .await
        .unwrap();
    let invocation_id = InvocationId::new("inv-1").unwrap();
    mark_unknown(&store, &invocation_id).await;

    let stale = store
        .transition(
            &invocation_id,
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::Succeeded {
                    result: StatusSafeResult::try_new(
                        json!({"ok": true}),
                        &descriptor(json!({
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["ok"],
                            "properties": {"ok": {"type": "boolean"}}
                        })),
                    )
                    .unwrap(),
                },
                TransitionAuditRecord::Outcome("audit-outcome-stale".to_owned()),
            )
            .unwrap(),
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
    let replacement = reserve_default(&store, reservation("inv-2", "key-2", 102))
        .await
        .unwrap();
    assert_eq!(replacement.kind(), ReservationKind::Reserved);
}

async fn execute_if_reserved(
    store: &InMemoryInvocationStore,
    input: InvocationReservationInput,
    effect_count: &AtomicUsize,
) -> kiteframe_provider::InvocationStatus {
    let reservation = reserve_default(store, input).await.unwrap();
    if reservation.kind() == ReservationKind::Reserved {
        effect_count.fetch_add(1, Ordering::SeqCst);
    }
    reservation.status().clone()
}

async fn mark_unknown(store: &InMemoryInvocationStore, invocation_id: &InvocationId) {
    store
        .transition(
            invocation_id,
            InvocationTransition::try_new(
                InvocationState::Reserved,
                InvocationState::Pending,
                TransitionAuditRecord::Authorization("audit-authz-unknown".to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .transition(
            invocation_id,
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::OutcomeUnknown,
                TransitionAuditRecord::None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

async fn reserve_default(
    store: &InMemoryInvocationStore,
    input: InvocationReservationInput,
) -> Result<kiteframe_provider::InvocationReservation, Diagnostic> {
    store
        .reserve_or_get(
            input,
            &descriptor(standard_output_schema()),
            Timestamp::new(u64::MAX),
        )
        .await
}

fn reservation(invocation_id: &str, key: &str, _untrusted_now: u64) -> InvocationReservationInput {
    InvocationReservationInput {
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
        authority_revision_digest: digest(5),
        status_context: status_context("human-7"),
        proposal_digest: digest(6),
        protected_evidence_refs: vec![
            ProtectedEvidenceRequestRef::new("evidence://approval-1").unwrap(),
        ],
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

fn descriptor(output_schema: serde_json::Value) -> CapabilityDescriptor {
    descriptor_with_retention(output_schema, 3_600)
}

fn descriptor_with_retention(
    output_schema: serde_json::Value,
    retention_seconds: u64,
) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability(),
        summary: "Update a case".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId"],
            "properties": {"caseId": {"type": "string"}}
        }),
        output_schema,
        stable_errors: vec![],
        execution_modes: NonEmptySet::try_new(BTreeSet::from([ExecutionMode::Deferred])).unwrap(),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({
            "type": "string",
            "pattern": "^case:[A-Za-z0-9-]+$"
        }))
        .unwrap(),
        effect: EffectClassification::ReversibleWrite,
        idempotency: IdempotencyRequirement::Required {
            scope: IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: std::num::NonZeroU64::new(retention_seconds).unwrap(),
        },
        freshness: FreshnessRequirement::default(),
        preconditions: vec![],
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn standard_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["caseId"],
        "properties": {"caseId": {"type": "string"}}
    })
}

struct FakeClock(AtomicU64);

impl InvocationStoreClock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp::new(self.0.load(Ordering::SeqCst))
    }
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}

fn test_suspension() -> Suspension {
    Suspension::try_new(
        CheckpointRef::new("checkpoint://test/01").unwrap(),
        EvidenceKind::Approval,
        ProtectedEvidenceRequestRef::new("evidence-request://test").unwrap(),
        digest(6),
    )
    .unwrap()
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
