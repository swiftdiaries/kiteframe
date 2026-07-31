use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, ApprovalRequirement, AuthorityRevision, AuthorityRevisionSet,
    CapabilityDescriptor, CapabilityDescriptorParts, CapabilityGrantSet, CapabilityGrantSetParts,
    CapabilityIdentity, CapabilityName, CapabilityReleaseVersion, ConfirmationRequirement,
    ConsentRequirement, EffectClassification, EffectProposal, EffectiveCapabilityGrant,
    EffectiveCapabilityGrantParts, EvidenceKind, EvidenceReferences, EvidenceRequirement,
    ExecutionMode, FreshnessRequirement, IdempotencyRequirement, IdempotencyScope, InvocationId,
    InvocationOutcome, InvocationRequest, LockedCapability, NonEmptySet,
    NormalizedResourceSelector, PreconditionDescriptor, PreconditionKind,
    ProtectedEvidenceRequestRef, RequiredEvidence, ResourceSelectorSchema, RetryClass, SessionRef,
    Sha256Digest, StableCapabilityError, Suspension, TaskRef, Timestamp, TraceContext,
};
use kiteframe_provider::{
    AdmissionAuthorizationRequest, AdmissionAuthorizationResult, AuditRecord, AuditSink,
    AuthorizationBackend, AuthorizationDecision, CapabilityOperation, DurableAuditReceipt,
    EffectAuditDigests, EffectEnforcementPlane, InMemoryInvocationAdmissionStore,
    InMemoryInvocationStore, InvocationAdmission, InvocationCheckpointIssuer, InvocationClock,
    InvocationContext, InvocationEventSink, InvocationEvidenceProvider, InvocationService,
    InvocationStoreClock, NarrowedAuthorizationConditions, OperationFailure, OperationRegistry,
    Precondition, ResumeRequest, SafeDenialReason, VerifiedEvidence, VerifiedHumanPrincipal,
    VerifiedProviderPrincipals, VerifiedWorkloadPrincipal,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn expired_grant_fails_before_authorization_or_operation() {
    let fixture = fixture(FixtureOptions {
        grant_expires_at: 150,
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-002");
    assert_eq!(
        fixture.events.snapshot(),
        ["validate_request", "validate_grant"]
    );
}

#[tokio::test]
async fn grant_maximum_effect_blocks_a_stronger_locked_operation_before_authorization() {
    let fixture = fixture(FixtureOptions {
        effect: EffectClassification::ReversibleWrite,
        grant_maximum_effect: Some(EffectClassification::ReadOnly),
        execution_modes: vec![ExecutionMode::Immediate],
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert_eq!(
        fixture.events.snapshot(),
        ["validate_request", "validate_grant"]
    );
}

#[tokio::test]
async fn read_operation_requires_immediate_mode_in_the_effective_grant() {
    let fixture = fixture(FixtureOptions {
        execution_modes: vec![ExecutionMode::Immediate, ExecutionMode::Deferred],
        grant_execution_modes: Some(vec![ExecutionMode::Deferred]),
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert_eq!(
        fixture.events.snapshot(),
        ["validate_request", "validate_grant"]
    );
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn stale_policy_fails_before_precondition_or_effect() {
    let fixture = fixture(FixtureOptions {
        current_revision: "r2",
        effect: EffectClassification::ReversibleWrite,
        execution_modes: vec![ExecutionMode::Immediate],
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-004");
    assert_eq!(
        fixture.events.snapshot(),
        [
            "validate_request",
            "validate_grant",
            "authenticate",
            "validate_freshness",
        ]
    );
}

#[tokio::test]
async fn effect_handoff_never_emits_an_unsupported_deferred_outcome() {
    let fixture = fixture(FixtureOptions {
        effect: EffectClassification::ReversibleWrite,
        execution_modes: vec![ExecutionMode::Immediate],
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-002");
    assert_eq!(fixture.events.snapshot().last(), Some(&"authorize"));
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn invalid_result_is_never_returned() {
    let fixture = fixture(FixtureOptions {
        operation_result: json!({"unexpected": true}),
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-002");
    assert!(fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn undeclared_stable_error_is_never_returned() {
    let fixture = fixture(FixtureOptions {
        operation_error: Some(
            StableCapabilityError::try_new(
                "CASE_OTHER",
                "case",
                RetryClass::Never,
                "undeclared stable error",
            )
            .unwrap(),
        ),
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-002");
    assert!(fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn tampered_locked_semantic_digest_fails_before_request_validation() {
    let fixture = fixture(FixtureOptions {
        tamper_locked_safety_digest: true,
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-002");
    assert!(fixture.events.snapshot().is_empty());
}

#[tokio::test]
async fn evidence_for_another_proposal_cannot_resume_effect() {
    let fixture = fixture(FixtureOptions::approval());
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };

    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("vault://approval/7").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-7",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );
    let resume_request = fixture.request(evidence_refs("approval-token", evidence_ref.as_str()));
    let wrong_suspension = Suspension::try_new(
        suspension.checkpoint_ref().clone(),
        suspension.evidence_kind(),
        suspension.evidence_request_ref().clone(),
        digest(99),
    )
    .unwrap();

    fixture.events.clear();
    let error = fixture
        .service
        .resume(ResumeRequest::new(resume_request, wrong_suspension))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(fixture.events.snapshot().is_empty());
}

#[tokio::test]
async fn changed_request_with_matching_new_evidence_cannot_resume_old_proposal() {
    let fixture = fixture(FixtureOptions::approval());
    let original = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(original).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let changed = fixture.request_with(
        "case:42",
        json!({"caseId": "99"}),
        BTreeMap::new(),
        EvidenceReferences::default(),
    );
    let changed_proposal = EffectProposal::try_new(&changed, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("evidence://approval/changed").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-8",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *changed_proposal.proposal_digest(),
        )
        .unwrap(),
    );

    fixture.events.clear();
    let error = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request_with(
                "case:42",
                json!({"caseId": "99"}),
                BTreeMap::new(),
                evidence_refs("approval-token", evidence_ref.as_str()),
            ),
            suspension.clone(),
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"validate_evidence"));
    assert!(!fixture.events.snapshot().contains(&"execute"));

    let consumed = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(EvidenceReferences::default()),
            suspension,
        ))
        .await
        .unwrap_err();
    assert_eq!(
        consumed.message.as_str(),
        "required evidence is still absent at resume"
    );
}

#[tokio::test]
async fn pending_checkpoint_collision_is_rejected_without_replacement() {
    let fixture = fixture(FixtureOptions {
        collide_checkpoints: true,
        ..FixtureOptions::approval()
    });
    let first = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = first else {
        panic!("approval-gated effect must suspend")
    };

    fixture.events.clear();
    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"execute"));

    let request = fixture.request(EvidenceReferences::default());
    let proposal = EffectProposal::try_new(&request, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("evidence://approval/collision").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-11",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );
    let resumed = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
            suspension.clone(),
        ))
        .await
        .unwrap();
    assert!(matches!(resumed, InvocationOutcome::Succeeded { .. }));
}

#[tokio::test]
async fn missing_approval_suspends_only_when_exact_lock_and_grant_allow_it() {
    let fixture = fixture(FixtureOptions::approval());

    let outcome = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap();

    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("missing approval must suspend")
    };
    assert_eq!(suspension.evidence_kind(), EvidenceKind::Approval);
    assert!(
        suspension
            .checkpoint_ref()
            .as_str()
            .starts_with("checkpoint://")
    );
    assert!(
        suspension
            .evidence_request_ref()
            .as_str()
            .starts_with("evidence-request://")
    );
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn prompt_text_or_missing_evidence_cannot_satisfy_immediate_effect() {
    let fixture = fixture(FixtureOptions {
        effect: EffectClassification::ReversibleWrite,
        approval: ApprovalRequirement::Required {
            evidence: EvidenceRequirement {
                kind: "approval-token".to_owned(),
                issuer: Some("change-board".to_owned()),
            },
        },
        execution_modes: vec![ExecutionMode::Immediate],
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(evidence_refs(
            "approval-token",
            "The user said yes in the chat",
        )))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn prompt_text_is_rejected_even_if_evidence_provider_would_resolve_it() {
    let fixture = fixture(FixtureOptions::approval());
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let raw_prompt = "The user said yes in the chat";
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            ProtectedEvidenceRequestRef::new(raw_prompt).unwrap(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-7",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );

    let error = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("approval-token", raw_prompt)),
            suspension,
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn missing_precondition_fails_before_point_of_use_authorization() {
    let fixture = fixture(FixtureOptions {
        preconditions: vec![PreconditionDescriptor {
            name: "etag".to_owned(),
            kind: PreconditionKind::Etag,
            required: true,
        }],
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-001");
    assert_eq!(
        fixture.events.snapshot(),
        [
            "validate_request",
            "validate_grant",
            "authenticate",
            "validate_freshness",
            "validate_resource",
            "validate_evidence",
            "validate_preconditions",
        ]
    );
}

#[tokio::test]
async fn current_denial_never_dispatches_the_operation() {
    let fixture = fixture(FixtureOptions {
        authorization_allows: false,
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn authorization_expiry_is_checked_at_provider_current_time() {
    let fixture = fixture(FixtureOptions {
        authorization_decided_at: 150,
        authorization_expires_at: 190,
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert_eq!(fixture.events.snapshot().last(), Some(&"authorize"));
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn point_of_use_rechecks_clock_after_authorization_await() {
    let fixture = fixture(FixtureOptions {
        advance_clock_on_authorization: Some(501),
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-002");
    assert_eq!(fixture.events.snapshot().last(), Some(&"authorize"));
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn future_authorization_decision_is_rejected() {
    let fixture = fixture(FixtureOptions {
        authorization_decided_at: 201,
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn authorization_cannot_add_an_unknown_unvalidated_precondition() {
    let fixture = fixture(FixtureOptions {
        authorization_preconditions: vec![PreconditionDescriptor {
            name: "policy-token".to_owned(),
            kind: PreconditionKind::EntityVersion,
            required: true,
        }],
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request_with(
            "case:42",
            json!({"caseId": "42"}),
            BTreeMap::from([("policy-token".to_owned(), "opaque".to_owned())]),
            EvidenceReferences::default(),
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn invalid_input_fails_before_grant_or_authorization_validation() {
    let fixture = fixture(FixtureOptions::read());

    let error = fixture
        .service
        .invoke(fixture.request_with(
            "case:42",
            json!({"wrong": true}),
            BTreeMap::new(),
            EvidenceReferences::default(),
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-002");
    assert_eq!(fixture.events.snapshot(), ["validate_request"]);
}

#[tokio::test]
async fn ungranted_or_nonconcrete_resource_fails_before_evidence_or_operation() {
    for resource in ["case:other", "case:*"] {
        let fixture = fixture(FixtureOptions::read());
        let error = fixture
            .service
            .invoke(fixture.request_with(
                resource,
                json!({"caseId": "42"}),
                BTreeMap::new(),
                EvidenceReferences::default(),
            ))
            .await
            .unwrap_err();

        assert_eq!(error.code.as_str(), "KF-AUTH-003");
        assert_eq!(
            fixture.events.snapshot(),
            [
                "validate_request",
                "validate_grant",
                "authenticate",
                "validate_freshness",
                "validate_resource",
            ]
        );
    }
}

#[tokio::test]
async fn persisted_actor_must_match_the_authenticated_human() {
    let fixture = fixture(FixtureOptions {
        authenticated_actor: "actor-other",
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert_eq!(
        fixture.events.snapshot(),
        ["validate_request", "validate_grant", "authenticate"]
    );
}

#[tokio::test]
async fn principal_verifier_diagnostic_is_redacted_at_the_trust_boundary() {
    let mut malicious = kiteframe_contract::Diagnostic::error(
        kiteframe_contract::DiagnosticCode::RuntimeConstruction,
        kiteframe_contract::DiagnosticCategory::Runtime,
        kiteframe_contract::DiagnosticStage::Runtime,
        "Authorization: Bearer secret-token-value",
    );
    malicious
        .details
        .insert("credential".to_owned(), json!("Bearer secret-token-value"));
    let fixture = fixture(FixtureOptions {
        principal_error: Some(malicious),
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert_eq!(
        error.message.as_str(),
        "authenticated principal verification failed"
    );
    assert!(error.details.is_empty());
    assert!(!format!("{error:?}").contains("secret-token-value"));
}

#[tokio::test]
async fn stale_operation_precondition_fails_before_authorization() {
    let fixture = fixture(FixtureOptions {
        preconditions: vec![PreconditionDescriptor {
            name: "etag".to_owned(),
            kind: PreconditionKind::Etag,
            required: true,
        }],
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request_with(
            "case:42",
            json!({"caseId": "42"}),
            BTreeMap::from([("etag".to_owned(), "stale".to_owned())]),
            EvidenceReferences::default(),
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-CAP-001");
    assert!(!fixture.events.snapshot().contains(&"authorize"));
}

#[tokio::test]
async fn stale_authorization_decision_revision_is_rejected_after_current_check() {
    let fixture = fixture(FixtureOptions {
        decision_revision: "r0",
        ..FixtureOptions::read()
    });

    let error = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-004");
    assert_eq!(fixture.events.snapshot().last(), Some(&"authorize"));
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn confirmation_is_bound_to_the_authenticated_human_subject() {
    let fixture = fixture(FixtureOptions {
        effect: EffectClassification::ReversibleWrite,
        execution_modes: vec![ExecutionMode::Suspendable],
        confirmation: ConfirmationRequirement::Required {
            evidence: EvidenceRequirement {
                kind: "confirmation-token".to_owned(),
                issuer: Some("confirmation-service".to_owned()),
            },
        },
        ..FixtureOptions::read()
    });
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("confirmation-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("vault://confirmation/7").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Confirmation,
            "confirmation-token",
            "human-other",
            Some("confirmation-service"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );

    let error = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("confirmation-token", evidence_ref.as_str())),
            suspension,
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn consent_remains_distinct_from_confirmation_and_approval() {
    let fixture = fixture(FixtureOptions {
        effect: EffectClassification::ReversibleWrite,
        execution_modes: vec![ExecutionMode::Suspendable],
        consent: ConsentRequirement::Required {
            evidence: EvidenceRequirement {
                kind: "consent-token".to_owned(),
                issuer: Some("consent-service".to_owned()),
            },
        },
        ..FixtureOptions::read()
    });

    let outcome = fixture
        .service
        .invoke(fixture.request(EvidenceReferences::default()))
        .await
        .unwrap();

    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("consent-gated effect must suspend")
    };
    assert_eq!(suspension.evidence_kind(), EvidenceKind::Consent);
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn resume_revalidates_principals_freshness_preconditions_and_authorization() {
    let fixture = fixture(FixtureOptions::approval());
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("vault://approval/8").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-8",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );

    fixture.events.clear();
    let outcome = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
            suspension.clone(),
        ))
        .await
        .unwrap();

    assert!(matches!(outcome, InvocationOutcome::Succeeded { .. }));
    assert_eq!(
        fixture.events.snapshot(),
        [
            "validate_request",
            "validate_grant",
            "authenticate",
            "validate_freshness",
            "validate_resource",
            "validate_evidence",
            "validate_preconditions",
            "authorize",
            "reserve",
            "audit_authorization",
            "execute",
            "audit_outcome",
            "terminal_status",
        ]
    );
}

#[tokio::test]
async fn evidence_expiry_is_rechecked_after_authorization_before_effect_handoff() {
    let fixture = fixture(FixtureOptions {
        advance_clock_on_authorization: Some(251),
        ..FixtureOptions::approval()
    });
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("evidence://approval/boundary").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-boundary",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );

    fixture.events.clear();
    let error = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
            suspension,
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-003");
    assert_eq!(fixture.events.snapshot().last(), Some(&"authorize"));
    assert!(!fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn concurrent_resume_allows_exactly_one_in_flight_continuation() {
    let fixture = fixture(FixtureOptions {
        yield_authorization_check: true,
        ..FixtureOptions::approval()
    });
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("evidence://approval/concurrent").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-concurrent",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );
    let resume = ResumeRequest::new(
        fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
        suspension,
    );

    let (first, second) = tokio::join!(
        fixture.service.resume(resume.clone()),
        fixture.service.resume(resume),
    );
    let mut succeeded = 0;
    let mut rejected = 0;
    for result in [first, second] {
        match result {
            Ok(InvocationOutcome::Succeeded { .. }) => succeeded += 1,
            Err(error) if error.code.as_str() == "KF-AUTH-003" => rejected += 1,
            other => panic!("unexpected concurrent resume result: {other:?}"),
        }
    }
    assert_eq!(succeeded, 1);
    assert_eq!(rejected, 1);
    assert_eq!(
        fixture
            .events
            .snapshot()
            .iter()
            .filter(|event| **event == "execute")
            .count(),
        1
    );
}

#[tokio::test]
async fn dropped_resume_future_restores_pending_checkpoint_for_retry() {
    let fixture = fixture(FixtureOptions {
        yield_authorization_check: true,
        ..FixtureOptions::approval()
    });
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("evidence://approval/cancelled").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-cancelled",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );
    let resume = ResumeRequest::new(
        fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
        suspension,
    );

    let mut abandoned = Box::pin(fixture.service.resume(resume.clone()));
    std::future::poll_fn(|context| match abandoned.as_mut().poll(context) {
        std::task::Poll::Pending => std::task::Poll::Ready(()),
        std::task::Poll::Ready(result) => {
            panic!("resume must yield before it is abandoned: {result:?}")
        }
    })
    .await;
    drop(abandoned);

    let retried = fixture.service.resume(resume).await.unwrap();
    assert!(matches!(retried, InvocationOutcome::Succeeded { .. }));
    assert!(fixture.events.snapshot().contains(&"execute"));
}

#[tokio::test]
async fn resume_rejects_authority_revision_change_before_evidence_or_authorization() {
    let fixture = fixture(FixtureOptions::approval());
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("evidence://approval/revision").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-9",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(600),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );
    *fixture.current_revision.lock().unwrap() = "r2";

    fixture.events.clear();
    let error = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
            suspension.clone(),
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-004");
    assert_eq!(
        fixture.events.snapshot().last(),
        Some(&"validate_freshness")
    );
    assert!(!fixture.events.snapshot().contains(&"validate_evidence"));

    *fixture.current_revision.lock().unwrap() = "r1";
    let retried = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
            suspension,
        ))
        .await
        .unwrap();
    assert!(matches!(retried, InvocationOutcome::Succeeded { .. }));
}

#[tokio::test]
async fn resume_rejects_grant_that_expired_while_suspended() {
    let fixture = fixture(FixtureOptions::approval());
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();
    let evidence_ref = ProtectedEvidenceRequestRef::new("evidence://approval/expiry").unwrap();
    fixture.evidence.insert(
        VerifiedEvidence::try_new(
            evidence_ref.clone(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-10",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(600),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    );
    fixture.clock.set(501);

    fixture.events.clear();
    let error = fixture
        .service
        .resume(ResumeRequest::new(
            fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
            suspension,
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_str(), "KF-AUTH-002");
    assert_eq!(
        fixture.events.snapshot(),
        ["validate_request", "validate_grant"]
    );
    assert!(!fixture.events.snapshot().contains(&"validate_evidence"));
}

#[tokio::test]
async fn evidence_is_bound_to_kind_subject_issuer_action_resource_and_time() {
    let fixture = fixture(FixtureOptions::approval());
    let missing = fixture.request(EvidenceReferences::default());
    let outcome = fixture.service.invoke(missing.clone()).await.unwrap();
    let InvocationOutcome::Suspended { suspension, .. } = outcome else {
        panic!("approval-gated effect must suspend")
    };
    let proposal = EffectProposal::try_new(&missing, fixture.descriptor()).unwrap();

    for (index, evidence) in [
        VerifiedEvidence::try_new(
            ProtectedEvidenceRequestRef::new("vault://wrong/kind").unwrap(),
            EvidenceKind::Confirmation,
            "approval-token",
            "approver-7",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
        VerifiedEvidence::try_new(
            ProtectedEvidenceRequestRef::new("vault://wrong/issuer").unwrap(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-7",
            Some("other-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
        VerifiedEvidence::try_new(
            ProtectedEvidenceRequestRef::new("vault://wrong/resource").unwrap(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-7",
            Some("change-board"),
            fixture.identity(),
            selector("case:other"),
            Timestamp::new(190),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
        VerifiedEvidence::try_new(
            ProtectedEvidenceRequestRef::new("vault://wrong/time").unwrap(),
            EvidenceKind::Approval,
            "approval-token",
            "approver-7",
            Some("change-board"),
            fixture.identity(),
            selector("case:42"),
            Timestamp::new(201),
            Timestamp::new(250),
            *proposal.proposal_digest(),
        )
        .unwrap(),
    ]
    .into_iter()
    .enumerate()
    {
        let evidence_ref = evidence.reference().clone();
        fixture.evidence.insert(evidence);
        let error = fixture
            .service
            .resume(ResumeRequest::new(
                fixture.request(evidence_refs("approval-token", evidence_ref.as_str())),
                suspension.clone(),
            ))
            .await
            .unwrap_err();
        assert_eq!(
            error.code.as_str(),
            "KF-AUTH-003",
            "invalid evidence case {index}"
        );
    }
}

#[derive(Clone)]
struct RecordingEvents(Arc<Mutex<Vec<&'static str>>>);

impl RecordingEvents {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn snapshot(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

impl InvocationEventSink for RecordingEvents {
    fn record(&self, event: &'static str) {
        self.0.lock().unwrap().push(event);
    }
}

struct FakeClock(AtomicU64);

impl InvocationClock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp::new(self.0.load(Ordering::SeqCst))
    }
}

impl InvocationStoreClock for FakeClock {
    fn now(&self) -> Timestamp {
        InvocationClock::now(self)
    }
}

struct TestAuditSink(AtomicU64);

#[async_trait]
impl AuditSink for TestAuditSink {
    async fn append(
        &self,
        record: AuditRecord,
    ) -> Result<DurableAuditReceipt, kiteframe_contract::Diagnostic> {
        let sequence = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        DurableAuditReceipt::try_new(
            record.partition(),
            sequence,
            digest(0),
            digest(sequence as u8),
        )
        .map_err(|message| {
            kiteframe_contract::Diagnostic::error(
                kiteframe_contract::DiagnosticCode::AuditUnavailable,
                kiteframe_contract::DiagnosticCategory::Audit,
                kiteframe_contract::DiagnosticStage::Invoke,
                message,
            )
        })
    }
}

impl FakeClock {
    fn set(&self, unix_seconds: u64) {
        self.0.store(unix_seconds, Ordering::SeqCst);
    }
}

struct FakeCheckpointIssuer {
    counter: AtomicU64,
    collide: bool,
}

impl InvocationCheckpointIssuer for FakeCheckpointIssuer {
    fn issue(
        &self,
        proposal: &EffectProposal,
    ) -> Result<kiteframe_contract::CheckpointRef, kiteframe_contract::Diagnostic> {
        let value = if self.collide {
            7
        } else {
            self.counter.fetch_add(1, Ordering::SeqCst)
        };
        kiteframe_contract::CheckpointRef::new(format!(
            "checkpoint://{}/{value:064x}",
            proposal.proposal_digest()
        ))
        .map_err(|message| {
            kiteframe_contract::Diagnostic::error(
                kiteframe_contract::DiagnosticCode::InvocationDenied,
                kiteframe_contract::DiagnosticCategory::Authorization,
                kiteframe_contract::DiagnosticStage::Invoke,
                message,
            )
        })
    }
}

struct FakePrincipalVerifier {
    admission_id: AdmissionId,
    actor: &'static str,
    error: Option<kiteframe_contract::Diagnostic>,
}

#[async_trait]
impl kiteframe_provider::ProviderPrincipalVerifier for FakePrincipalVerifier {
    async fn verify(&self) -> Result<VerifiedProviderPrincipals, kiteframe_contract::Diagnostic> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(VerifiedProviderPrincipals::new(
            VerifiedHumanPrincipal::try_new(
                "tenant-1",
                "human-7",
                ActorRef::new(self.actor).unwrap(),
                Timestamp::new(600),
            )
            .unwrap(),
            VerifiedWorkloadPrincipal::try_new(
                "tenant-1",
                "workload-2",
                "run-9",
                AgentRef::new("agent-2").unwrap(),
                TaskRef::new("task-4").unwrap(),
                SessionRef::new("session-3").unwrap(),
                self.admission_id.clone(),
                Timestamp::new(600),
            )
            .unwrap(),
        ))
    }
}

struct FakeAuthorizationBackend {
    revision: Arc<Mutex<&'static str>>,
    decision_revision: &'static str,
    allows: bool,
    decided_at: Timestamp,
    expires_at: Timestamp,
    required_preconditions: Vec<PreconditionDescriptor>,
    clock: Arc<FakeClock>,
    advance_clock_to: Option<u64>,
    yield_check: bool,
}

#[async_trait]
impl AuthorizationBackend for FakeAuthorizationBackend {
    async fn list_admissible(
        &self,
        request: &AdmissionAuthorizationRequest,
    ) -> Result<AdmissionAuthorizationResult, kiteframe_contract::Diagnostic> {
        Ok(AdmissionAuthorizationResult::new(vec![
            request.capability().clone(),
        ]))
    }

    async fn check(
        &self,
        request: &kiteframe_provider::InvocationAuthorizationRequest,
    ) -> Result<AuthorizationDecision, kiteframe_contract::Diagnostic> {
        if self.yield_check {
            yield_once().await;
        }
        if let Some(unix_seconds) = self.advance_clock_to {
            self.clock.set(unix_seconds);
        }
        if !self.allows {
            return Ok(AuthorizationDecision::deny(
                "decision-deny",
                SafeDenialReason::ResourceDenied,
            )
            .unwrap());
        }
        Ok(AuthorizationDecision::allow(
            "decision-allow",
            revisions(self.decision_revision),
            self.decided_at,
            NarrowedAuthorizationConditions::new(
                vec![request.selected_resource().clone()],
                self.expires_at,
                self.required_preconditions.clone(),
            )
            .unwrap(),
        )
        .unwrap())
    }

    async fn revisions(&self) -> Result<AuthorityRevisionSet, kiteframe_contract::Diagnostic> {
        Ok(revisions(*self.revision.lock().unwrap()))
    }
}

async fn yield_once() {
    let mut yielded = false;
    std::future::poll_fn(|context| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await
}

struct FakeOperation {
    identity: CapabilityIdentity,
    result: Value,
    error: Option<StableCapabilityError>,
}

#[async_trait]
impl CapabilityOperation for FakeOperation {
    fn identity(&self) -> &CapabilityIdentity {
        &self.identity
    }

    async fn validate_preconditions(
        &self,
        _context: &InvocationContext,
        preconditions: &[Precondition],
    ) -> Result<(), kiteframe_contract::Diagnostic> {
        if preconditions
            .iter()
            .any(|precondition| precondition.name() == "etag" && precondition.value() != "v7")
        {
            return Err(kiteframe_contract::Diagnostic::error(
                kiteframe_contract::DiagnosticCode::PreconditionMissing,
                kiteframe_contract::DiagnosticCategory::Capability,
                kiteframe_contract::DiagnosticStage::Invoke,
                "etag precondition is stale",
            ));
        }
        Ok(())
    }

    async fn execute(
        &self,
        _context: &InvocationContext,
        _arguments: Value,
    ) -> Result<Value, OperationFailure> {
        if let Some(error) = &self.error {
            return Err(OperationFailure::Stable(error.clone()));
        }
        Ok(self.result.clone())
    }
}

#[derive(Default)]
struct FakeEvidenceProvider {
    records: Mutex<BTreeMap<String, VerifiedEvidence>>,
}

impl FakeEvidenceProvider {
    fn insert(&self, evidence: VerifiedEvidence) {
        self.records
            .lock()
            .unwrap()
            .insert(evidence.reference().as_str().to_owned(), evidence);
    }
}

#[async_trait]
impl InvocationEvidenceProvider for FakeEvidenceProvider {
    async fn resolve(
        &self,
        reference: &ProtectedEvidenceRequestRef,
    ) -> Result<VerifiedEvidence, kiteframe_contract::Diagnostic> {
        self.records
            .lock()
            .unwrap()
            .get(reference.as_str())
            .cloned()
            .ok_or_else(|| {
                kiteframe_contract::Diagnostic::error(
                    kiteframe_contract::DiagnosticCode::InvocationDenied,
                    kiteframe_contract::DiagnosticCategory::Authorization,
                    kiteframe_contract::DiagnosticStage::Invoke,
                    "evidence reference was not verified",
                )
            })
    }
}

struct Fixture {
    service: InvocationService,
    evidence: Arc<FakeEvidenceProvider>,
    events: RecordingEvents,
    identity: CapabilityIdentity,
    descriptor: CapabilityDescriptor,
    admission_id: AdmissionId,
    grant_digest: Sha256Digest,
    clock: Arc<FakeClock>,
    current_revision: Arc<Mutex<&'static str>>,
}

impl Fixture {
    fn identity(&self) -> CapabilityIdentity {
        self.identity.clone()
    }

    fn descriptor(&self) -> &CapabilityDescriptor {
        &self.descriptor
    }

    fn request(&self, evidence_refs: EvidenceReferences) -> InvocationRequest {
        self.request_with(
            "case:42",
            json!({"caseId": "42"}),
            BTreeMap::new(),
            evidence_refs,
        )
    }

    fn request_with(
        &self,
        selected_resource: &str,
        arguments: Value,
        preconditions: BTreeMap<String, String>,
        evidence_refs: EvidenceReferences,
    ) -> InvocationRequest {
        InvocationRequest::try_new(
            InvocationId::new("invocation-7").unwrap(),
            self.admission_id.clone(),
            self.grant_digest,
            digest(6),
            self.identity.clone(),
            selected_resource,
            arguments,
            preconditions,
            (self.descriptor.effect() != EffectClassification::ReadOnly)
                .then_some("idempotency-7".to_owned()),
            evidence_refs,
            trace_context(),
        )
        .unwrap()
    }
}

#[derive(Clone)]
struct FixtureOptions {
    grant_expires_at: u64,
    current_revision: &'static str,
    decision_revision: &'static str,
    authorization_allows: bool,
    authorization_decided_at: u64,
    authorization_expires_at: u64,
    authorization_preconditions: Vec<PreconditionDescriptor>,
    advance_clock_on_authorization: Option<u64>,
    yield_authorization_check: bool,
    authenticated_actor: &'static str,
    principal_error: Option<kiteframe_contract::Diagnostic>,
    effect: EffectClassification,
    grant_maximum_effect: Option<EffectClassification>,
    execution_modes: Vec<ExecutionMode>,
    grant_execution_modes: Option<Vec<ExecutionMode>>,
    confirmation: ConfirmationRequirement,
    approval: ApprovalRequirement,
    consent: ConsentRequirement,
    preconditions: Vec<PreconditionDescriptor>,
    operation_result: Value,
    operation_error: Option<StableCapabilityError>,
    tamper_locked_safety_digest: bool,
    collide_checkpoints: bool,
}

impl FixtureOptions {
    fn read() -> Self {
        Self {
            grant_expires_at: 500,
            current_revision: "r1",
            decision_revision: "r1",
            authorization_allows: true,
            authorization_decided_at: 200,
            authorization_expires_at: 400,
            authorization_preconditions: vec![],
            advance_clock_on_authorization: None,
            yield_authorization_check: false,
            authenticated_actor: "actor-7",
            principal_error: None,
            effect: EffectClassification::ReadOnly,
            grant_maximum_effect: None,
            execution_modes: vec![ExecutionMode::Immediate],
            grant_execution_modes: None,
            confirmation: ConfirmationRequirement::None,
            approval: ApprovalRequirement::None,
            consent: ConsentRequirement::None,
            preconditions: vec![],
            operation_result: json!({"caseId": "42", "summary": "stable"}),
            operation_error: None,
            tamper_locked_safety_digest: false,
            collide_checkpoints: false,
        }
    }

    fn approval() -> Self {
        Self {
            effect: EffectClassification::ReversibleWrite,
            execution_modes: vec![ExecutionMode::Deferred, ExecutionMode::Suspendable],
            approval: ApprovalRequirement::Required {
                evidence: EvidenceRequirement {
                    kind: "approval-token".to_owned(),
                    issuer: Some("change-board".to_owned()),
                },
            },
            ..Self::read()
        }
    }
}

fn fixture(options: FixtureOptions) -> Fixture {
    let identity = capability_identity();
    let descriptor = descriptor(&identity, &options);
    let [
        input_digest,
        output_digest,
        stable_errors_digest,
        mut safety_digest,
    ] = descriptor_part_digests(&descriptor);
    if options.tamper_locked_safety_digest {
        safety_digest = digest(99);
    }
    let locked = LockedCapability::try_new(
        identity.clone(),
        descriptor.clone(),
        *descriptor.descriptor_digest(),
        input_digest,
        output_digest,
        stable_errors_digest,
        safety_digest,
    )
    .unwrap();
    let grant = EffectiveCapabilityGrant::try_new(EffectiveCapabilityGrantParts {
        capability: identity.clone(),
        resources: vec![selector("case:42")],
        execution_modes: modes(
            options
                .grant_execution_modes
                .as_deref()
                .unwrap_or(&options.execution_modes),
        ),
        maximum_effect: options.grant_maximum_effect.unwrap_or(options.effect),
        expires_at: Timestamp::new(options.grant_expires_at),
        required_evidence: RequiredEvidence::new(
            options.confirmation.clone(),
            options.approval.clone(),
            options.consent.clone(),
        ),
        freshness: FreshnessRequirement {
            max_admission_age_seconds: None,
            policy_revision_required: true,
            max_input_age_seconds: None,
        },
        preconditions: options.preconditions.clone(),
    })
    .unwrap();
    let admission_id = AdmissionId::new("admission-5").unwrap();
    let grant_set = CapabilityGrantSet::try_new(CapabilityGrantSetParts {
        admission_id: admission_id.clone(),
        admission_request_digest: digest(1),
        delegation_ancestry_digest: digest(6),
        actor: ActorRef::new("actor-7").unwrap(),
        agent: AgentRef::new("agent-2").unwrap(),
        task: TaskRef::new("task-4").unwrap(),
        session: SessionRef::new("session-3").unwrap(),
        policy_revision: kiteframe_contract::PolicyRevision::new("policy-r1").unwrap(),
        catalog_identity: kiteframe_contract::CatalogIdentity {
            name: "provider.catalog".to_owned(),
            revision: "1.0.0".to_owned(),
        },
        catalog_digest: digest(10),
        authority_revisions: revisions("r1"),
        issued_at: Timestamp::new(100),
        expires_at: Timestamp::new(550),
        grants: vec![grant],
        optional_denials: vec![],
    })
    .unwrap();
    let grant_digest = *grant_set.grant_digest();
    let admission = InvocationAdmission::try_new(grant_set, vec![locked]).unwrap();
    let admissions = Arc::new(InMemoryInvocationAdmissionStore::new(vec![admission]).unwrap());
    let evidence = Arc::new(FakeEvidenceProvider::default());
    let events = RecordingEvents::new();
    let mut registry = OperationRegistry::new();
    registry
        .register(FakeOperation {
            identity: identity.clone(),
            result: options.operation_result,
            error: options.operation_error,
        })
        .unwrap();
    let clock = Arc::new(FakeClock(AtomicU64::new(200)));
    let current_revision = Arc::new(Mutex::new(options.current_revision));
    let registry = registry
        .freeze(Arc::new(FakeAuthorizationBackend {
            revision: current_revision.clone(),
            decision_revision: options.decision_revision,
            allows: options.authorization_allows,
            decided_at: Timestamp::new(options.authorization_decided_at),
            expires_at: Timestamp::new(options.authorization_expires_at),
            required_preconditions: options.authorization_preconditions.clone(),
            clock: clock.clone(),
            advance_clock_to: options.advance_clock_on_authorization,
            yield_check: options.yield_authorization_check,
        }))
        .unwrap();
    let enforcement = EffectEnforcementPlane::new(
        Arc::new(InMemoryInvocationStore::with_clock(clock.clone())),
        Arc::new(TestAuditSink(AtomicU64::new(0))),
        EffectAuditDigests::new(digest(20), digest(21), digest(22), digest(23)),
    );
    let service = InvocationService::try_new(
        admissions,
        Arc::new(FakePrincipalVerifier {
            admission_id: admission_id.clone(),
            actor: options.authenticated_actor,
            error: options.principal_error,
        }),
        registry,
        evidence.clone(),
        clock.clone(),
        Arc::new(FakeCheckpointIssuer {
            counter: AtomicU64::new(1),
            collide: options.collide_checkpoints,
        }),
    )
    .unwrap()
    .with_event_sink(Arc::new(events.clone()))
    .with_effect_enforcement(enforcement);

    Fixture {
        service,
        evidence,
        events,
        identity,
        descriptor,
        admission_id,
        grant_digest,
        clock,
        current_revision,
    }
}

fn descriptor(identity: &CapabilityIdentity, options: &FixtureOptions) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: identity.clone(),
        summary: "Read or update a stable case projection".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId"],
            "properties": {"caseId": {"type": "string"}},
        }),
        output_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["caseId", "summary"],
            "properties": {
                "caseId": {"type": "string"},
                "summary": {"type": "string"},
            },
        }),
        stable_errors: vec![],
        execution_modes: modes(&options.execution_modes),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({
            "type": "string",
            "pattern": "^case:[A-Za-z0-9-]+$"
        }))
        .unwrap(),
        effect: options.effect,
        idempotency: if options.effect == EffectClassification::ReadOnly {
            IdempotencyRequirement::None
        } else {
            IdempotencyRequirement::Required {
                scope: IdempotencyScope::ActorCapabilityResourceOperation,
                retention_seconds: std::num::NonZeroU64::new(3600).unwrap(),
            }
        },
        freshness: FreshnessRequirement {
            max_admission_age_seconds: None,
            policy_revision_required: true,
            max_input_age_seconds: None,
        },
        preconditions: options.preconditions.clone(),
        confirmation: options.confirmation.clone(),
        approval: options.approval.clone(),
        consent: options.consent.clone(),
    })
    .unwrap()
}

fn capability_identity() -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new("cases.update").unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn selector(value: &str) -> NormalizedResourceSelector {
    NormalizedResourceSelector::new(value).unwrap()
}

fn modes(values: &[ExecutionMode]) -> NonEmptySet<ExecutionMode> {
    NonEmptySet::try_new(values.iter().copied().collect::<BTreeSet<_>>()).unwrap()
}

fn revisions(revision: &str) -> AuthorityRevisionSet {
    AuthorityRevisionSet::try_new(vec![
        AuthorityRevision::try_new("policy", revision).unwrap(),
    ])
    .unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}

fn evidence_refs(kind: &str, reference: &str) -> EvidenceReferences {
    EvidenceReferences::try_new(BTreeMap::from([(
        kind.to_owned(),
        Value::String(reference.to_owned()),
    )]))
    .unwrap()
}

fn trace_context() -> TraceContext {
    TraceContext::try_new(
        "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
        None,
        BTreeMap::new(),
    )
    .unwrap()
}

fn descriptor_part_digests(descriptor: &CapabilityDescriptor) -> [Sha256Digest; 4] {
    let wire = serde_json::to_value(descriptor).unwrap();
    let object = wire.as_object().unwrap();
    let mut safety = serde_json::Map::new();
    for name in [
        "executionModes",
        "resourceSelectorSchema",
        "effect",
        "idempotency",
        "freshness",
        "preconditions",
        "confirmation",
        "approval",
        "consent",
    ] {
        safety.insert(name.to_owned(), object[name].clone());
    }
    [
        descriptor_part_digest("input-schema", &object["inputSchema"]),
        descriptor_part_digest("output-schema", &object["outputSchema"]),
        descriptor_part_digest("stable-errors", &object["stableErrors"]),
        descriptor_part_digest("safety-metadata", &Value::Object(safety)),
    ]
}

fn descriptor_part_digest(domain: &str, value: &Value) -> Sha256Digest {
    let canonical = serde_json_canonicalizer::to_vec(value).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(b"kiteframe.dev/capability-descriptor/");
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(canonical);
    Sha256Digest::from_bytes(hasher.finalize().into())
}
