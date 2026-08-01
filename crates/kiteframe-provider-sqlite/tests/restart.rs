use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use kiteframe_contract::{
    ActorRef, AdmissionId, ApprovalRequirement, CapabilityDescriptor, CapabilityDescriptorParts,
    CapabilityIdentity, CapabilityName, CapabilityReleaseVersion, CatalogIdentity, CheckpointRef,
    ConfirmationRequirement, ConsentRequirement, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, EffectClassification, EvidenceKind, ExecutionMode, FreshnessRequirement,
    IdempotencyKey, IdempotencyRequirement, IdempotencyScope, InvocationId, NonEmptySet,
    NormalizedResourceSelector, ProtectedEvidenceRequestRef, ResourceSelectorSchema, RetryClass,
    Sha256Digest, StableCapabilityError, StatusRequest, Suspension, Timestamp, TraceContext,
};
use kiteframe_provider::{
    AbandonmentAuthorization, IdempotencyScopeValue, InvocationAuditLinkKind,
    InvocationReservationInput, InvocationState, InvocationStatusContext, InvocationStore,
    InvocationStoreClock, InvocationTransition, ReservationKind, StatusSafeError, StatusSafeResult,
    StatusState, TransitionAuditRecord,
};
use kiteframe_provider_sqlite::SqliteInvocationStore;
use serde_json::json;
use sqlx::{Connection, Row};

#[tokio::test]
async fn status_survives_restart_with_digests_trace_and_unknown_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invocations.sqlite3");
    let clock = Arc::new(FakeClock::new(100));
    let store = SqliteInvocationStore::open_with_clock(&path, clock)
        .await
        .unwrap();
    let descriptor = descriptor(standard_output_schema(), 3_600);
    let input = reservation("inv-1", "key-1", "human-7");
    store
        .reserve_or_get(input.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    mark_unknown(&store, &input.invocation_id).await;
    drop(store);

    let reopened = SqliteInvocationStore::open(&path).await.unwrap();
    let status = reopened
        .status(
            &StatusRequest::new(input.invocation_id, trace_context("a")),
            &status_context("human-7"),
        )
        .await
        .unwrap();

    assert_eq!(status.status_state(), StatusState::OutcomeUnknown);
    assert_eq!(status.request_digest(), &digest(1));
    assert_eq!(status.grant_digest(), &digest(2));
    assert_eq!(status.catalog_identity(), &input.catalog_identity);
    assert_eq!(status.catalog_digest(), &digest(3));
    assert_eq!(status.descriptor_digest(), descriptor.descriptor_digest());
    assert_eq!(status.authority_revision_digest(), &digest(5));
    assert_eq!(status.proposal_digest(), &digest(6));
    assert_eq!(
        reopened.last_traceparent().as_deref(),
        Some("00-0123456789abcdef0123456789abcdef-aaaaaaaaaaaaaaaa-01")
    );
}

#[tokio::test]
async fn restart_fences_pending_effect_until_authorized_abandonment() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pending-recovery.sqlite3");
    let clock = Arc::new(FakeClock::new(100));
    let descriptor = descriptor(standard_output_schema(), 3_600);
    let first = reservation("inv-pending", "key-pending", "human-7");
    let store = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    store
        .reserve_or_get(first.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    attach_authorization(&store, &first.invocation_id, "audit-authz-pending").await;
    drop(store);

    clock.set(101);
    let reopened = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    let recovered = status(&reopened, &first.invocation_id, "9").await;
    assert_eq!(recovered.state(), &InvocationState::OutcomeUnknown);
    assert_eq!(
        recovered.audit_authorization_record_id(),
        Some("audit-authz-pending")
    );

    let error = reopened
        .reserve_or_get(
            reservation("inv-retry", "key-retry", "human-7"),
            &descriptor,
            Timestamp::new(3_701),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-003");

    let unauthorized = reopened
        .abandon(
            &first.invocation_id,
            &status_context("human-8"),
            AbandonmentAuthorization::try_new("audit-abandon-denied", "operator-8").unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(unauthorized.code.as_str(), "KF-AUTH-003");
    let still_blocked = reopened
        .reserve_or_get(
            reservation("inv-retry", "key-retry", "human-7"),
            &descriptor,
            Timestamp::new(3_701),
        )
        .await
        .unwrap_err();
    assert_eq!(still_blocked.code.as_str(), "KF-CAP-003");

    reopened
        .abandon(
            &first.invocation_id,
            &status_context("human-7"),
            AbandonmentAuthorization::try_new("audit-abandon-approved", "operator-3").unwrap(),
        )
        .await
        .unwrap();
    let replacement = reopened
        .reserve_or_get(
            reservation("inv-retry", "key-retry", "human-7"),
            &descriptor,
            Timestamp::new(3_701),
        )
        .await
        .unwrap();
    assert_eq!(replacement.kind(), ReservationKind::Reserved);
}

#[tokio::test]
async fn restart_also_fences_a_durable_reserved_effect() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reserved-recovery.sqlite3");
    let clock = Arc::new(FakeClock::new(100));
    let descriptor = descriptor(standard_output_schema(), 3_600);
    let first = reservation("inv-reserved", "key-reserved", "human-7");
    let store = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    store
        .reserve_or_get(first.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    drop(store);

    clock.set(101);
    let reopened = SqliteInvocationStore::open_with_clock(&path, clock)
        .await
        .unwrap();
    let recovered = status(&reopened, &first.invocation_id, "8").await;

    assert_eq!(recovered.state(), &InvocationState::OutcomeUnknown);
    let denied = reopened
        .reserve_or_get(
            reservation("inv-retry", "key-retry", "human-7"),
            &descriptor,
            Timestamp::new(3_701),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code.as_str(), "KF-CAP-003");
}

#[tokio::test]
async fn concurrent_duplicate_reservation_has_one_owner() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(100));
    let store = Arc::new(
        SqliteInvocationStore::open_with_clock(directory.path().join("concurrent.sqlite3"), clock)
            .await
            .unwrap(),
    );
    let left_store = store.clone();
    let right_store = store.clone();
    let left = tokio::spawn(async move {
        left_store
            .reserve_or_get(
                reservation("inv-1", "key-1", "human-7"),
                &descriptor(standard_output_schema(), 3_600),
                Timestamp::new(3_700),
            )
            .await
            .unwrap()
    });
    let right = tokio::spawn(async move {
        right_store
            .reserve_or_get(
                reservation("inv-2", "key-1", "human-7"),
                &descriptor(standard_output_schema(), 3_600),
                Timestamp::new(3_700),
            )
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
async fn status_and_duplicate_dedupe_require_the_original_exact_context() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(100));
    let store =
        SqliteInvocationStore::open_with_clock(directory.path().join("auth.sqlite3"), clock)
            .await
            .unwrap();
    let descriptor = descriptor(standard_output_schema(), 3_600);
    let input = reservation("inv-1", "key-1", "human-7");
    store
        .reserve_or_get(input.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    let request = StatusRequest::new(input.invocation_id, trace_context("b"));

    for mismatch in mismatched_contexts() {
        let error = store.status(&request, &mismatch).await.unwrap_err();
        assert_eq!(error.code.as_str(), "KF-AUTH-003");
    }

    let error = store
        .reserve_or_get(
            reservation("inv-2", "key-1", "human-8"),
            &descriptor,
            Timestamp::new(3_700),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUTH-003");
}

#[tokio::test]
async fn descriptor_retention_and_cleanup_use_only_trusted_store_time() {
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(100));
    let store = SqliteInvocationStore::open_with_clock(
        directory.path().join("retention.sqlite3"),
        clock.clone(),
    )
    .await
    .unwrap();
    let descriptor = descriptor(standard_output_schema(), 3_600);

    let short = store
        .reserve_or_get(
            reservation("inv-short", "key-short", "human-7"),
            &descriptor,
            Timestamp::new(3_699),
        )
        .await
        .unwrap_err();
    assert_eq!(short.code.as_str(), "KF-CAP-002");

    let first = reservation("inv-1", "key-1", "human-7");
    store
        .reserve_or_get(first.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    complete_success(&store, &first.invocation_id, &descriptor).await;

    clock.set(3_699);
    let retained = store
        .reserve_or_get(
            reservation("inv-2", "key-1", "human-7"),
            &descriptor,
            Timestamp::new(7_299),
        )
        .await
        .unwrap();
    assert_eq!(retained.kind(), ReservationKind::Existing);
    assert_eq!(retained.status().invocation_id().as_str(), "inv-1");

    clock.set(3_700);
    let replacement = store
        .reserve_or_get(
            reservation("inv-3", "key-1", "human-7"),
            &descriptor,
            Timestamp::new(7_300),
        )
        .await
        .unwrap();
    assert_eq!(replacement.kind(), ReservationKind::Reserved);
    assert_eq!(replacement.status().created_at(), Timestamp::new(3_700));

    clock.set(5_000);
    let stale_future_deadline = store
        .reserve_or_get(
            reservation("inv-4", "key-4", "human-7"),
            &descriptor,
            Timestamp::new(6_000),
        )
        .await
        .unwrap_err();
    assert_eq!(stale_future_deadline.code.as_str(), "KF-CAP-002");
}

#[tokio::test]
async fn unknown_outcome_survives_retention_and_restart_until_authorized_abandonment() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("abandon.sqlite3");
    let clock = Arc::new(FakeClock::new(1));
    let store = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    let descriptor = descriptor(standard_output_schema(), 1);
    let first = reservation("inv-1", "key-1", "human-7");
    store
        .reserve_or_get(first.clone(), &descriptor, Timestamp::new(2))
        .await
        .unwrap();
    mark_unknown(&store, &first.invocation_id).await;
    clock.set(3);
    drop(store);

    let reopened = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    let error = reopened
        .reserve_or_get(
            reservation("inv-2", "key-2", "human-7"),
            &descriptor,
            Timestamp::new(4),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-003");

    reopened
        .abandon(
            &first.invocation_id,
            &status_context("human-7"),
            AbandonmentAuthorization::try_new("audit-abandon-9", "operator-3").unwrap(),
        )
        .await
        .unwrap();
    drop(reopened);

    let reopened = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    let status = reopened
        .status(
            &StatusRequest::new(first.invocation_id.clone(), trace_context("c")),
            &status_context("human-7"),
        )
        .await
        .unwrap();
    assert_eq!(
        status.abandonment_authorization_record_id(),
        Some("audit-abandon-9")
    );
    assert_eq!(status.abandoned_by(), Some("operator-3"));

    let replacement = reopened
        .reserve_or_get(
            reservation("inv-2", "key-2", "human-7"),
            &descriptor,
            Timestamp::new(4),
        )
        .await
        .unwrap();
    assert_eq!(replacement.kind(), ReservationKind::Reserved);
}

#[tokio::test]
async fn every_unresolved_effect_state_fences_its_scope_before_and_after_expired_restart() {
    for (index, state) in [
        InvocationState::Reserved,
        InvocationState::Pending,
        InvocationState::Suspended {
            suspension: Box::new(test_suspension()),
        },
        InvocationState::OutcomeUnknown,
    ]
    .into_iter()
    .enumerate()
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("unresolved-{index}.sqlite3"));
        let clock = Arc::new(FakeClock::new(1));
        let descriptor = descriptor(standard_output_schema(), 1);
        let first = reservation("inv-unresolved", "key-unresolved", "human-7");
        let store = SqliteInvocationStore::open_with_clock(&path, clock.clone())
            .await
            .unwrap();
        store
            .reserve_or_get(first.clone(), &descriptor, Timestamp::new(2))
            .await
            .unwrap();
        transition_to_unresolved(&store, &first.invocation_id, &state).await;

        let before_expiration = store
            .reserve_or_get(
                reservation("inv-before-expiration", "key-before-expiration", "human-7"),
                &descriptor,
                Timestamp::new(2),
            )
            .await
            .unwrap_err();
        assert_eq!(
            before_expiration.code.as_str(),
            "KF-CAP-003",
            "state {state:?} must fence a different key before expiration"
        );

        clock.set(3);
        let after_expiration = store
            .reserve_or_get(
                reservation("inv-after-expiration", "key-after-expiration", "human-7"),
                &descriptor,
                Timestamp::new(4),
            )
            .await
            .unwrap_err();
        assert_eq!(
            after_expiration.code.as_str(),
            "KF-CAP-003",
            "state {state:?} must fence a different key after expiration"
        );
        drop(store);

        let reopened = SqliteInvocationStore::open_with_clock(&path, clock.clone())
            .await
            .unwrap();
        let after_restart = reopened
            .reserve_or_get(
                reservation("inv-after-restart", "key-after-restart", "human-7"),
                &descriptor,
                Timestamp::new(4),
            )
            .await
            .unwrap_err();
        assert_eq!(
            after_restart.code.as_str(),
            "KF-CAP-003",
            "state {state:?} must fence a different key after expired restart"
        );
    }
}

#[tokio::test]
async fn exact_suspension_checkpoint_and_proposal_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("suspension.sqlite3");
    let clock = Arc::new(FakeClock::new(100));
    let descriptor = descriptor(standard_output_schema(), 3_600);
    let input = reservation("inv-suspended", "key-suspended", "human-7");
    let suspension = test_suspension();
    let store = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    store
        .reserve_or_get(input.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    store
        .transition(
            &input.invocation_id,
            InvocationTransition::try_new(
                InvocationState::Reserved,
                InvocationState::Suspended {
                    suspension: Box::new(suspension.clone()),
                },
                TransitionAuditRecord::None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteInvocationStore::open_with_clock(&path, clock)
        .await
        .unwrap();
    let status = reopened
        .status(
            &StatusRequest::new(input.invocation_id.clone(), trace_context("a")),
            &status_context("human-7"),
        )
        .await
        .unwrap();

    assert!(matches!(
        status.state(),
        InvocationState::Suspended { suspension: stored } if stored.as_ref() == &suspension
    ));
    assert!(matches!(
        status.portable().unwrap(),
        kiteframe_contract::InvocationStatus::Suspended { suspension: stored, .. }
            if stored == suspension
    ));
}

#[tokio::test]
async fn audit_ids_attach_with_transitions_and_safe_terminal_data_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("terminal.sqlite3");
    let clock = Arc::new(FakeClock::new(100));
    let store = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    let descriptor = descriptor(standard_output_schema(), 3_600);
    let success = reservation("inv-success", "key-success", "human-7");
    let reserved = store
        .reserve_or_get(success.clone(), &descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    assert_eq!(reserved.status().audit_authorization_record_id(), None);
    assert_eq!(reserved.status().audit_outcome_record_id(), None);

    clock.set(101);
    attach_authorization(&store, &success.invocation_id, "audit-authz-success").await;
    let pending = status(&store, &success.invocation_id, "d").await;
    assert_eq!(
        pending.audit_authorization_record_id(),
        Some("audit-authz-success")
    );
    assert_eq!(pending.audit_outcome_record_id(), None);

    clock.set(102);
    store
        .transition(
            &success.invocation_id,
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::Succeeded {
                    result: StatusSafeResult::try_new(json!({"caseId": "42"}), &descriptor)
                        .unwrap(),
                },
                TransitionAuditRecord::Outcome("audit-outcome-success".to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let failure = reservation("inv-failure", "key-failure", "human-7");
    store
        .reserve_or_get(failure.clone(), &descriptor, Timestamp::new(3_702))
        .await
        .unwrap();
    attach_authorization(&store, &failure.invocation_id, "audit-authz-failure").await;
    let stable_error = StableCapabilityError::try_new(
        "CASE_CONFLICT",
        "conflict",
        RetryClass::AfterRefresh,
        "authorizationHeader: signed opaque material",
    )
    .unwrap();
    store
        .transition(
            &failure.invocation_id,
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::Failed {
                    error: StatusSafeError::try_from_stable(&stable_error).unwrap(),
                },
                TransitionAuditRecord::Outcome("audit-outcome-failure".to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteInvocationStore::open_with_clock(&path, clock)
        .await
        .unwrap();
    let success_status = status(&reopened, &success.invocation_id, "e").await;
    let InvocationState::Succeeded { result } = success_status.state() else {
        panic!("expected safe succeeded state")
    };
    assert_eq!(result.value(), &json!({}));
    assert_eq!(
        success_status.audit_authorization_record_id(),
        Some("audit-authz-success")
    );
    assert_eq!(
        success_status.audit_outcome_record_id(),
        Some("audit-outcome-success")
    );
    let failure_status = status(&reopened, &failure.invocation_id, "f").await;
    let InvocationState::Failed { error } = failure_status.state() else {
        panic!("expected safe failed state")
    };
    assert_eq!(error.code(), "CASE_CONFLICT");
    assert_eq!(error.message(), "capability invocation failed");
    assert!(
        !serde_json::to_string(error)
            .unwrap()
            .contains("signed opaque material")
    );
    assert_eq!(
        failure_status.audit_outcome_record_id(),
        Some("audit-outcome-failure")
    );
}

#[tokio::test]
async fn audit_history_survives_resume_unknown_and_terminal_resolution_in_order() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("audit-history.sqlite3");
    let clock = Arc::new(FakeClock::new(100));
    let store = SqliteInvocationStore::open_with_clock(&path, clock.clone())
        .await
        .unwrap();
    let descriptor = descriptor(standard_output_schema(), 3_600);
    let input = reservation("inv-history", "key-history", "human-7");
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
        clock.set(101 + offset as u64);
        store
            .transition(
                &input.invocation_id,
                InvocationTransition::try_new(expected, next, audit).unwrap(),
            )
            .await
            .unwrap();
    }
    drop(store);

    let reopened = SqliteInvocationStore::open_with_clock(&path, clock)
        .await
        .unwrap();
    let current = status(&reopened, &input.invocation_id, "7").await;
    let links = current
        .audit_links()
        .iter()
        .map(|link| (link.kind(), link.record_id(), link.attached_at()))
        .collect::<Vec<_>>();
    assert_eq!(
        links,
        vec![
            (
                InvocationAuditLinkKind::Authorization,
                "audit-authz-initial",
                Timestamp::new(101),
            ),
            (
                InvocationAuditLinkKind::Authorization,
                "audit-authz-resumed",
                Timestamp::new(103),
            ),
            (
                InvocationAuditLinkKind::Outcome,
                "audit-outcome-unknown",
                Timestamp::new(104),
            ),
            (
                InvocationAuditLinkKind::Outcome,
                "audit-outcome-final",
                Timestamp::new(105),
            ),
        ]
    );
}

#[tokio::test]
async fn sensitive_result_and_diagnostic_content_never_reaches_persistence_or_status() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sensitive.sqlite3");
    let clock = Arc::new(FakeClock::new(100));
    let store = SqliteInvocationStore::open_with_clock(&path, clock)
        .await
        .unwrap();
    let sensitive_descriptor = descriptor(
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["apiKey", "authorizationHeader", "note", "changed"],
            "properties": {
                "apiKey": {"type": "string"},
                "authorizationHeader": {"type": "string"},
                "note": {"type": "string"},
                "changed": {"type": "boolean"}
            }
        }),
        3_600,
    );
    let input = reservation("inv-sensitive", "key-sensitive", "human-7");
    store
        .reserve_or_get(input.clone(), &sensitive_descriptor, Timestamp::new(3_700))
        .await
        .unwrap();
    attach_authorization(&store, &input.invocation_id, "audit-authz-sensitive").await;

    let safe_result = StatusSafeResult::try_new(
        json!({
            "apiKey": "key-without-an-existing-marker",
            "authorizationHeader": "signed opaque material",
            "note": "sk_live_not-covered-by-the-old-denylist",
            "changed": true
        }),
        &sensitive_descriptor,
    )
    .unwrap();
    assert_eq!(safe_result.value(), &json!({"changed": true}));
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
    let safe_error = StatusSafeError::try_from_diagnostic(&diagnostic).unwrap();
    assert_eq!(safe_error.message(), "invocation was denied");
    let error_wire = serde_json::to_string(&safe_error).unwrap();
    assert!(!error_wire.contains("sk_live"));
    assert!(!error_wire.contains("signed opaque material"));
    store
        .transition(
            &input.invocation_id,
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::Succeeded {
                    result: safe_result,
                },
                TransitionAuditRecord::Outcome("audit-outcome-sensitive".to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let url = format!("sqlite://{}", path.display());
    let mut connection = sqlx::SqliteConnection::connect(&url).await.unwrap();
    let row = sqlx::query(
        "SELECT state_kind, state_json FROM invocations
         WHERE invocation_id = 'inv-sensitive'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("state_kind"), "succeeded");
    let state_json = row.get::<String, _>("state_json");
    for forbidden in [
        "key-without-an-existing-marker",
        "signed opaque material",
        "sk_live_not-covered-by-the-old-denylist",
    ] {
        assert!(!state_json.contains(forbidden));
    }
    let current = status(&store, &input.invocation_id, "1").await;
    let InvocationState::Succeeded { result } = current.state() else {
        panic!("expected safe succeeded state")
    };
    assert_eq!(result.value(), &json!({"changed": true}));
    assert_eq!(
        current.audit_outcome_record_id(),
        Some("audit-outcome-sensitive")
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
        "retention_until",
    ] {
        assert!(columns.iter().any(|column| column == required));
    }
    for removed_singleton in ["audit_authorization_record_id", "audit_outcome_record_id"] {
        assert!(!columns.iter().any(|column| column == removed_singleton));
    }

    let audit_link_columns = sqlx::query("PRAGMA table_info(invocation_audit_links)")
        .fetch_all(&mut connection)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<Vec<_>>();
    assert_eq!(
        audit_link_columns,
        [
            "invocation_id",
            "sequence",
            "kind",
            "record_id",
            "attached_at"
        ]
    );
}

async fn attach_authorization(
    store: &SqliteInvocationStore,
    invocation_id: &InvocationId,
    record_id: &str,
) {
    store
        .transition(
            invocation_id,
            InvocationTransition::try_new(
                InvocationState::Reserved,
                InvocationState::Pending,
                TransitionAuditRecord::Authorization(record_id.to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

async fn complete_success(
    store: &SqliteInvocationStore,
    invocation_id: &InvocationId,
    descriptor: &CapabilityDescriptor,
) {
    attach_authorization(store, invocation_id, "audit-authz-complete").await;
    store
        .transition(
            invocation_id,
            InvocationTransition::try_new(
                InvocationState::Pending,
                InvocationState::Succeeded {
                    result: StatusSafeResult::try_new(json!({"caseId": "42"}), descriptor).unwrap(),
                },
                TransitionAuditRecord::Outcome("audit-outcome-complete".to_owned()),
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

async fn mark_unknown(store: &SqliteInvocationStore, invocation_id: &InvocationId) {
    attach_authorization(store, invocation_id, "audit-authz-unknown").await;
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

async fn transition_to_unresolved(
    store: &SqliteInvocationStore,
    invocation_id: &InvocationId,
    target: &InvocationState,
) {
    match target {
        InvocationState::Reserved => {}
        InvocationState::Pending => {
            attach_authorization(store, invocation_id, "audit-authz-pending-state").await;
        }
        InvocationState::Suspended { suspension } => {
            store
                .transition(
                    invocation_id,
                    InvocationTransition::try_new(
                        InvocationState::Reserved,
                        InvocationState::Suspended {
                            suspension: suspension.clone(),
                        },
                        TransitionAuditRecord::None,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
        }
        InvocationState::OutcomeUnknown => mark_unknown(store, invocation_id).await,
        _ => panic!("test helper received a terminal invocation state"),
    }
}

async fn status(
    store: &SqliteInvocationStore,
    invocation_id: &InvocationId,
    trace_nibble: &str,
) -> kiteframe_provider::InvocationStatus {
    store
        .status(
            &StatusRequest::new(invocation_id.clone(), trace_context(trace_nibble)),
            &status_context("human-7"),
        )
        .await
        .unwrap()
}

fn reservation(invocation_id: &str, key: &str, human: &str) -> InvocationReservationInput {
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
        status_context: status_context(human),
        proposal_digest: digest(6),
        protected_evidence_refs: vec![
            ProtectedEvidenceRequestRef::new("evidence://approval-1").unwrap(),
        ],
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

fn mismatched_contexts() -> Vec<InvocationStatusContext> {
    (0..9)
        .map(|index| {
            let mut values = [
                "tenant-1",
                "human-7",
                "workload-2",
                "run-9",
                "actor-7",
                "agent-2",
                "task-4",
                "session-3",
                "admission-5",
            ];
            values[index] = [
                "tenant-2",
                "human-8",
                "workload-3",
                "run-8",
                "actor-8",
                "agent-3",
                "task-5",
                "session-4",
                "admission-6",
            ][index];
            context(values)
        })
        .collect()
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

fn descriptor(output_schema: serde_json::Value, retention_seconds: u64) -> CapabilityDescriptor {
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

struct FakeClock(AtomicU64);

impl FakeClock {
    fn new(now: u64) -> Self {
        Self(AtomicU64::new(now))
    }

    fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl InvocationStoreClock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp::new(self.0.load(Ordering::SeqCst))
    }
}
