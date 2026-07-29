use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use kiteframe_contract::{
    ActorRef, AdmissionId, AdmissionRequest, AdmissionRequestParts, AgentRef, ApprovalRequirement,
    CapabilityDescriptor, CapabilityDescriptorParts, CapabilityGrant, CapabilityGrantParts,
    CapabilityGrantSet, CapabilityGrantSetParts, CapabilityIdentity, CapabilityName,
    CapabilityReleaseVersion, CatalogRequest, ConfirmationRequirement, ConsentRequirement,
    DelegationAncestry, Diagnostic, EffectClassification, EvidenceReferences, EvidenceRequirement,
    ExecutionMode, IdempotencyRequirement, IdempotencyScope, InvocationId, InvocationOutcome,
    InvocationRequest, InvocationStatus, LockedCapability, NonEmptySet, NormalizedResourceSelector,
    PolicyRevision, PreconditionDescriptor, RequestedCapability, ResolvedCapabilityRequirement,
    ResourceSelectorSchema, RetryClass, SessionRef, Sha256Digest, StableCapabilityError,
    StatusFirstDiagnostic, Suspension, TaskRef, Timestamp, TraceContext,
};
use serde_json::json;

#[test]
fn grant_set_is_time_bounded_and_not_a_bearer_credential() {
    let schema = serde_json::to_string(&schemars::schema_for!(
        kiteframe_contract::CapabilityGrantSet
    ))
    .unwrap();
    assert!(schema.contains("issuedAt"));
    assert!(schema.contains("expiresAt"));
    assert!(schema.contains("policyRevision"));
    assert!(!schema.contains("token"));
    assert!(!schema.contains("credential"));
}

#[test]
fn effectful_invocation_requires_an_idempotency_key() {
    let descriptor = effectful_descriptor();
    let request = invocation_request(None);
    let errors = request.validate_against(&descriptor).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn outcome_unknown_requires_status_first_retry() {
    let outcome = InvocationOutcome::outcome_unknown(
        InvocationId::new("inv-1").unwrap(),
        Diagnostic::outcome_unknown("status is required"),
    )
    .unwrap();
    assert_eq!(outcome.diagnostic().unwrap().retry, RetryClass::StatusFirst);
}

#[test]
fn status_first_diagnostic_rejects_a_non_status_first_retry() {
    let diagnostic = Diagnostic::error(
        kiteframe_contract::DiagnosticCode::OutcomeUnknown,
        kiteframe_contract::DiagnosticCategory::Capability,
        kiteframe_contract::DiagnosticStage::Invoke,
        "status is required",
    );
    assert!(StatusFirstDiagnostic::try_new(diagnostic).is_err());
}

#[test]
fn status_first_diagnostic_schema_requires_status_first_retry() {
    let schema = serde_json::to_value(schemars::schema_for!(InvocationStatus)).unwrap();
    let status_first = &schema["$defs"]["StatusFirstDiagnostic"];

    assert_eq!(
        status_first["properties"]["retry"]["const"],
        json!("status_first")
    );
}

#[test]
fn catalog_request_deserialization_uses_validated_trace_context() {
    let request = CatalogRequest::new(
        Some(digest(9)),
        TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap(),
    );
    let decoded =
        serde_json::from_value::<CatalogRequest>(serde_json::to_value(request).unwrap()).unwrap();
    assert_eq!(decoded.known_catalog_digest(), Some(&digest(9)));
}

#[test]
fn invocation_request_deserialization_rejects_raw_evidence_payloads() {
    let mut wire = serde_json::to_value(invocation_request(Some("idem-1"))).unwrap();
    wire["evidenceRefs"] = json!({"approval": {"signedBy": "alice"}});
    assert!(serde_json::from_value::<InvocationRequest>(wire).is_err());
}

#[test]
fn invocation_status_schema_has_only_the_stable_status_vocabulary() {
    let schema = serde_json::to_string(&schemars::schema_for!(InvocationStatus)).unwrap();
    for status in [
        "pending",
        "suspended",
        "succeeded",
        "failed",
        "denied",
        "outcome_unknown",
    ] {
        assert!(schema.contains(status));
    }
    assert!(!schema.contains("deferred"));
}

#[test]
fn trace_context_rejects_non_allowlisted_baggage() {
    let baggage = BTreeMap::from([(String::from("credential"), String::from("secret"))]);
    assert!(TraceContext::try_new(valid_traceparent(), None, baggage).is_err());
}

#[test]
fn trace_context_deserialization_rejects_non_allowlisted_baggage() {
    let trace = serde_json::from_value::<TraceContext>(json!({
        "traceparent": valid_traceparent(),
        "baggage": {"authorization": "tuple"}
    }));
    assert!(trace.is_err());
}

#[test]
fn trace_context_rejects_an_opaque_bearer_secret_in_allowlisted_baggage() {
    let baggage = BTreeMap::from([(
        String::from("kiteframe.session_id"),
        String::from("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhbGljZSJ9.signature"),
    )]);
    assert!(TraceContext::try_new(valid_traceparent(), None, baggage).is_err());
}

#[test]
fn trace_context_rejects_noncanonical_traceparent_values() {
    for traceparent in [
        valid_traceparent().to_uppercase(),
        valid_traceparent().replacen("00-", "01-", 1),
        valid_traceparent().replacen("00-", "ff-", 1),
        String::from("00-00000000000000000000000000000000-00f067aa0ba902b7-01"),
        String::from("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"),
    ] {
        assert!(
            TraceContext::try_new(traceparent, None, BTreeMap::new()).is_err(),
            "invalid traceparent was accepted"
        );
    }
}

#[test]
fn trace_context_enforces_the_canonical_tracestate_subset() {
    for tracestate in [
        "vendor=value\r\nforged=member",
        "vendor=one,vendor=two",
        "1vendor=value",
        "vendor=value, next=two",
        "vendor=välue",
    ] {
        assert!(
            TraceContext::try_new(
                valid_traceparent(),
                Some(tracestate.to_owned()),
                BTreeMap::new(),
            )
            .is_err(),
            "invalid tracestate was accepted: {tracestate:?}"
        );
    }

    assert!(
        TraceContext::try_new(
            valid_traceparent(),
            Some(String::from("vendor=value,1tenant@system=value two")),
            BTreeMap::new(),
        )
        .is_ok()
    );
}

#[test]
fn trace_context_schema_expresses_native_wire_bounds() {
    let schema = serde_json::to_value(schemars::schema_for!(CatalogRequest)).unwrap();
    let trace = &schema["$defs"]["TraceContext"]["properties"];

    assert_eq!(
        trace["traceparent"]["pattern"],
        json!("^00-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$")
    );
    assert_eq!(trace["tracestate"]["minLength"], json!(1));
    assert_eq!(trace["tracestate"]["maxLength"], json!(512));
    assert_eq!(trace["tracestate"]["pattern"], json!("^[ -~]+$"));
}

#[test]
fn wire_deserialization_cannot_bypass_service_value_invariants() {
    assert!(
        serde_json::from_value::<DelegationAncestry>(json!([
            "agent:case-worker",
            "agent:case-worker"
        ]))
        .is_err()
    );

    let requested = serde_json::from_value::<RequestedCapability>(json!({
        "capability": {
            "name": "cases.comment",
            "version": "1.0.0"
        },
        "resources": [
            "tenant:t1/case:case-2",
            "tenant:t1/case:case-1",
            "tenant:t1/case:case-1"
        ]
    }))
    .unwrap();
    assert_eq!(
        requested
            .resources()
            .iter()
            .map(NormalizedResourceSelector::as_str)
            .collect::<Vec<_>>(),
        vec!["tenant:t1/case:case-1", "tenant:t1/case:case-2"]
    );

    for wire in [
        json!({
            "code": "",
            "category": "capability",
            "retry": "never",
            "message": "safe"
        }),
        json!({
            "code": "stable.error",
            "category": " ",
            "retry": "never",
            "message": "safe"
        }),
    ] {
        assert!(serde_json::from_value::<StableCapabilityError>(wire).is_err());
    }
    assert!(serde_json::from_value::<Suspension>(json!({"checkpointRef": "   "})).is_err());
}

#[test]
fn service_schemas_express_non_bypassable_collection_and_text_invariants() {
    let admission = serde_json::to_value(schemars::schema_for!(AdmissionRequest)).unwrap();
    assert_eq!(
        admission["$defs"]["DelegationAncestry"]["uniqueItems"],
        json!(true)
    );
    assert_eq!(
        admission["$defs"]["RequestedCapability"]["properties"]["resources"]["uniqueItems"],
        json!(true)
    );

    let outcome = serde_json::to_value(schemars::schema_for!(InvocationOutcome)).unwrap();
    assert_eq!(
        outcome["$defs"]["StableCapabilityError"]["properties"]["code"]["minLength"],
        json!(1)
    );
    assert_eq!(
        outcome["$defs"]["StableCapabilityError"]["properties"]["category"]["minLength"],
        json!(1)
    );
    assert_eq!(
        outcome["$defs"]["Suspension"]["properties"]["checkpointRef"]["minLength"],
        json!(1)
    );
}

#[test]
fn checked_in_admission_schema_requires_the_exact_locked_capability() {
    let schema: serde_json::Value = serde_json::from_slice(
        &fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/v1alpha1/admission-request.schema.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let requirement = &schema["$defs"]["ResolvedCapabilityRequirement"];
    let locked_capability = &schema["$defs"]["LockedCapability"];

    assert_eq!(
        requirement["required"],
        json!(["lockedCapability", "required", "resources"])
    );
    assert_eq!(
        requirement["properties"]["lockedCapability"],
        json!({"$ref": "#/$defs/LockedCapability"})
    );
    assert_eq!(
        locked_capability["required"],
        json!([
            "identity",
            "descriptor",
            "descriptorDigest",
            "inputSchemaDigest",
            "outputSchemaDigest",
            "stableErrorSetDigest",
            "safetyMetadataDigest"
        ])
    );
}

#[test]
fn evidence_references_reject_raw_payloads() {
    assert!(
        EvidenceReferences::try_new(BTreeMap::from([(
            String::from("approval"),
            json!({"signedBy": "alice"}),
        )]))
        .is_err()
    );
}

#[test]
fn idempotency_key_is_forbidden_when_descriptor_does_not_support_it() {
    let descriptor = read_only_descriptor();
    let request = invocation_request(Some("idem-1"));
    let errors = request.validate_against(&descriptor).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn grant_set_rejects_expiry_at_or_before_issue_time() {
    let mut parts = grant_set_parts();
    parts.expires_at = parts.issued_at;
    let errors = CapabilityGrantSet::try_new(parts).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn grant_set_rejects_duplicate_capability_versions() {
    let mut parts = grant_set_parts();
    parts.grants.push(parts.grants[0].clone());
    let errors = CapabilityGrantSet::try_new(parts).unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn grant_set_validation_rejects_mismatched_admission_identity() {
    let request = valid_admission_request();

    for parts in [
        CapabilityGrantSetParts {
            actor: ActorRef::new("actor:bob").unwrap(),
            ..grant_set_parts()
        },
        CapabilityGrantSetParts {
            agent: AgentRef::new("agent:other").unwrap(),
            ..grant_set_parts()
        },
        CapabilityGrantSetParts {
            task: TaskRef::new("task:other").unwrap(),
            ..grant_set_parts()
        },
        CapabilityGrantSetParts {
            session: SessionRef::new("session:other").unwrap(),
            ..grant_set_parts()
        },
    ] {
        let response = CapabilityGrantSet::try_new(parts).unwrap();
        let error = response.validate_against(&request).unwrap_err();
        assert_eq!(error.code.as_str(), "KF-CAP-002");
    }
}

#[test]
fn grant_set_validation_rejects_unrequested_or_broader_grants() {
    let request = valid_admission_request();

    let mut unrequested = grant_set_parts();
    unrequested.grants = vec![
        CapabilityGrant::try_new(CapabilityGrantParts {
            capability: capability_identity_with_name("cases.close"),
            resources: vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
        })
        .unwrap(),
    ];
    let response = CapabilityGrantSet::try_new(unrequested).unwrap();
    let error = response.validate_against(&request).unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-002");

    let mut broader = grant_set_parts();
    broader.grants = vec![
        CapabilityGrant::try_new(CapabilityGrantParts {
            capability: capability_identity(),
            resources: vec![NormalizedResourceSelector::new("tenant:t1/case:*").unwrap()],
        })
        .unwrap(),
    ];
    let response = CapabilityGrantSet::try_new(broader).unwrap();
    let error = response.validate_against(&request).unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-002");
}

#[test]
fn invocation_outcome_validation_rejects_a_different_invocation() {
    let request = invocation_request(None);
    let descriptor = read_only_descriptor();
    let outcome = InvocationOutcome::Succeeded {
        invocation_id: InvocationId::new("inv-other").unwrap(),
        result: json!({"ok": true}),
    };

    let error = outcome.validate_against(&request, &descriptor).unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-002");
}

#[test]
fn invocation_status_validation_rejects_a_different_invocation() {
    let request = invocation_request(None);
    let descriptor = read_only_descriptor();
    let status = InvocationStatus::Pending {
        invocation_id: InvocationId::new("inv-other").unwrap(),
    };

    let error = status.validate_against(&request, &descriptor).unwrap_err();
    assert_eq!(error.code.as_str(), "KF-CAP-002");
}

#[test]
fn succeeded_result_must_match_the_locked_output_schema() {
    let descriptor = response_descriptor(BTreeSet::from([ExecutionMode::Immediate]));
    let request = invocation_request(None);
    let outcome = InvocationOutcome::Succeeded {
        invocation_id: request.invocation_id().clone(),
        result: json!({"unexpected": true}),
    };

    let diagnostic = outcome.validate_against(&request, &descriptor).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "KF-CAP-002");
}

#[test]
fn failed_error_must_be_declared_by_the_locked_descriptor() {
    let descriptor = response_descriptor(BTreeSet::from([ExecutionMode::Immediate]));
    let request = invocation_request(None);
    let outcome = InvocationOutcome::Failed {
        invocation_id: request.invocation_id().clone(),
        error: stable_error("PROVIDER_NATIVE_500"),
    };

    let diagnostic = outcome.validate_against(&request, &descriptor).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "KF-CAP-002");
}

#[test]
fn deferred_and_suspended_variants_require_their_locked_execution_modes() {
    let descriptor = response_descriptor(BTreeSet::from([ExecutionMode::Immediate]));
    let request = invocation_request(None);
    let deferred = InvocationOutcome::Deferred {
        invocation_id: request.invocation_id().clone(),
    };
    let suspended = InvocationOutcome::Suspended {
        invocation_id: request.invocation_id().clone(),
        suspension: Suspension::try_new("checkpoint://case-1").unwrap(),
    };

    assert_eq!(
        deferred
            .validate_against(&request, &descriptor)
            .unwrap_err()
            .code
            .as_str(),
        "KF-CAP-002"
    );
    assert_eq!(
        suspended
            .validate_against(&request, &descriptor)
            .unwrap_err()
            .code
            .as_str(),
        "KF-CAP-002"
    );
}

#[test]
fn terminal_status_is_validated_against_the_locked_descriptor() {
    let descriptor = response_descriptor(BTreeSet::from([ExecutionMode::Immediate]));
    let request = invocation_request(None);
    let status = InvocationStatus::Succeeded {
        invocation_id: request.invocation_id().clone(),
        result: json!({"unexpected": true}),
    };

    let diagnostic = status.validate_against(&request, &descriptor).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "KF-CAP-002");
}

#[test]
fn denied_outcome_and_status_require_the_exact_canonical_diagnostic_tuple() {
    let descriptor = response_descriptor(BTreeSet::from([ExecutionMode::Immediate]));
    let request = invocation_request(None);
    let canonical = Diagnostic::error(
        kiteframe_contract::DiagnosticCode::InvocationDenied,
        kiteframe_contract::DiagnosticCategory::Authorization,
        kiteframe_contract::DiagnosticStage::Invoke,
        "invocation denied",
    );

    let valid_outcome = InvocationOutcome::Denied {
        invocation_id: request.invocation_id().clone(),
        diagnostic: canonical.clone(),
    };
    let valid_status = InvocationStatus::Denied {
        invocation_id: request.invocation_id().clone(),
        diagnostic: canonical.clone(),
    };
    assert!(
        valid_outcome
            .validate_against(&request, &descriptor)
            .is_ok()
    );
    assert!(valid_status.validate_against(&request, &descriptor).is_ok());

    for (field, diagnostic) in contradictory_denial_diagnostics(canonical) {
        let outcome = InvocationOutcome::Denied {
            invocation_id: request.invocation_id().clone(),
            diagnostic: diagnostic.clone(),
        };
        let status = InvocationStatus::Denied {
            invocation_id: request.invocation_id().clone(),
            diagnostic,
        };
        assert!(
            outcome.validate_against(&request, &descriptor).is_err(),
            "denied outcome accepted mismatched {field}"
        );
        assert!(
            status.validate_against(&request, &descriptor).is_err(),
            "denied status accepted mismatched {field}"
        );
    }
}

#[test]
fn outcome_unknown_requires_the_exact_canonical_diagnostic_tuple() {
    let canonical = Diagnostic::outcome_unknown("status is required");
    assert!(
        InvocationOutcome::outcome_unknown(InvocationId::new("inv-1").unwrap(), canonical.clone())
            .is_ok()
    );
    assert!(
        InvocationStatus::outcome_unknown(InvocationId::new("inv-1").unwrap(), canonical.clone())
            .is_ok()
    );

    for (field, diagnostic) in contradictory_status_first_diagnostics(canonical) {
        assert!(
            InvocationOutcome::outcome_unknown(
                InvocationId::new("inv-1").unwrap(),
                diagnostic.clone()
            )
            .is_err(),
            "outcome accepted mismatched {field}"
        );
        assert!(
            InvocationStatus::outcome_unknown(InvocationId::new("inv-1").unwrap(), diagnostic)
                .is_err(),
            "status accepted mismatched {field}"
        );
    }
}

#[test]
fn admission_rejects_resource_selector_broader_than_resolved_requirement() {
    let request = RequestedCapability::try_new(
        capability_identity(),
        vec![NormalizedResourceSelector::new("tenant:t1/case:*").unwrap()],
    )
    .unwrap();
    let errors = AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        required_capabilities: vec![request],
        optional_capabilities: Vec::new(),
        resolved_requirements: vec![resolved_requirement()],
        delegation_ancestry: DelegationAncestry::default(),
        contextual_facts: BTreeMap::new(),
        trace_context: TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap(),
    })
    .unwrap_err();
    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn admission_deserialization_rejects_duplicate_capability_versions() {
    let mut wire = serde_json::to_value(valid_admission_request()).unwrap();
    let required = wire["requiredCapabilities"].as_array_mut().unwrap();
    required.push(required[0].clone());
    assert!(serde_json::from_value::<AdmissionRequest>(wire).is_err());
}

#[test]
fn admission_deserialization_rejects_a_selector_broader_than_its_resolved_requirement() {
    let mut wire = serde_json::to_value(valid_admission_request()).unwrap();
    wire["requiredCapabilities"][0]["resources"] = json!(["tenant:t1/case:*"]);
    let error = serde_json::from_value::<AdmissionRequest>(wire).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("broader than the resolved requirement")
    );
}

#[test]
fn capability_grant_deserialization_rejects_empty_resources() {
    let mut wire = serde_json::to_value(grant_set_parts().grants.remove(0)).unwrap();
    wire["resources"] = json!([]);
    assert!(serde_json::from_value::<CapabilityGrant>(wire).is_err());
}

#[test]
fn outcome_unknown_deserialization_rejects_non_status_first_retry() {
    let wire = json!({
        "status": "outcome_unknown",
        "invocation_id": "inv-1",
        "diagnostic": diagnostic_with_retry("never")
    });
    assert!(serde_json::from_value::<InvocationOutcome>(wire).is_err());
}

#[test]
fn status_unknown_deserialization_rejects_non_status_first_retry() {
    let wire = json!({
        "status": "outcome_unknown",
        "invocation_id": "inv-1",
        "diagnostic": diagnostic_with_retry("never")
    });
    assert!(serde_json::from_value::<InvocationStatus>(wire).is_err());
}

fn invocation_request(idempotency_key: Option<&str>) -> InvocationRequest {
    InvocationRequest::try_new(
        InvocationId::new("inv-1").unwrap(),
        AdmissionId::new("adm-1").unwrap(),
        capability_identity(),
        "tenant:t1/case:case-1",
        json!({"caseId": "case-1"}),
        BTreeMap::new(),
        idempotency_key.map(ToOwned::to_owned),
        EvidenceReferences::try_new(BTreeMap::from([(
            String::from("approval"),
            json!("evidence://approval/1"),
        )]))
        .unwrap(),
        TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap(),
    )
    .unwrap()
}

fn effectful_descriptor() -> CapabilityDescriptor {
    descriptor(
        EffectClassification::ExternalSideEffect,
        IdempotencyRequirement::Required {
            scope: IdempotencyScope::ActorCapabilityResourceOperation,
            retention_seconds: std::num::NonZeroU64::new(60).unwrap(),
        },
    )
}

fn read_only_descriptor() -> CapabilityDescriptor {
    descriptor(EffectClassification::ReadOnly, IdempotencyRequirement::None)
}

fn descriptor(
    effect: EffectClassification,
    idempotency: IdempotencyRequirement,
) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability_identity(),
        summary: String::from("Operate on a case"),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        stable_errors: Vec::new(),
        execution_modes: NonEmptySet::try_new(BTreeSet::from([ExecutionMode::Immediate])).unwrap(),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect,
        idempotency,
        freshness: Default::default(),
        preconditions: Vec::<PreconditionDescriptor>::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::Required {
            evidence: EvidenceRequirement {
                kind: String::from("approval"),
                issuer: None,
            },
        },
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn response_descriptor(execution_modes: BTreeSet<ExecutionMode>) -> CapabilityDescriptor {
    CapabilityDescriptor::try_new(CapabilityDescriptorParts {
        identity: capability_identity(),
        summary: String::from("Operate on a case"),
        input_schema: json!({
            "type": "object",
            "required": ["caseId"],
            "properties": {"caseId": {"type": "string"}},
            "additionalProperties": false
        }),
        output_schema: json!({
            "type": "object",
            "required": ["accepted", "caseId"],
            "properties": {
                "accepted": {"type": "boolean"},
                "caseId": {"type": "string"}
            },
            "additionalProperties": false
        }),
        stable_errors: vec![
            kiteframe_contract::CapabilityErrorDescriptor::try_new(
                "CASE_CONFLICT",
                "conflict",
                "after_refresh",
                "case changed",
            )
            .unwrap(),
        ],
        execution_modes: NonEmptySet::try_new(execution_modes).unwrap(),
        resource_selector_schema: ResourceSelectorSchema::try_new(json!({"type": "string"}))
            .unwrap(),
        effect: EffectClassification::ReadOnly,
        idempotency: IdempotencyRequirement::None,
        freshness: Default::default(),
        preconditions: Vec::new(),
        confirmation: ConfirmationRequirement::None,
        approval: ApprovalRequirement::None,
        consent: ConsentRequirement::None,
    })
    .unwrap()
}

fn stable_error(code: &str) -> StableCapabilityError {
    StableCapabilityError::try_new(code, "conflict", RetryClass::AfterRefresh, "case changed")
        .unwrap()
}

fn contradictory_denial_diagnostics(canonical: Diagnostic) -> Vec<(&'static str, Diagnostic)> {
    let mut code = canonical.clone();
    code.code = kiteframe_contract::DiagnosticCode::AdmissionDenied;
    let mut category = canonical.clone();
    category.category = kiteframe_contract::DiagnosticCategory::Capability;
    let mut severity = canonical.clone();
    severity.severity = kiteframe_contract::DiagnosticSeverity::Warning;
    let mut stage = canonical.clone();
    stage.stage = kiteframe_contract::DiagnosticStage::Admit;
    let mut retry = canonical;
    retry.retry = RetryClass::AfterRefresh;
    vec![
        ("code", code),
        ("category", category),
        ("severity", severity),
        ("stage", stage),
        ("retry", retry),
    ]
}

fn contradictory_status_first_diagnostics(
    canonical: Diagnostic,
) -> Vec<(&'static str, Diagnostic)> {
    let mut code = canonical.clone();
    code.code = kiteframe_contract::DiagnosticCode::ResultInvalid;
    let mut category = canonical.clone();
    category.category = kiteframe_contract::DiagnosticCategory::Authorization;
    let mut severity = canonical.clone();
    severity.severity = kiteframe_contract::DiagnosticSeverity::Warning;
    let mut stage = canonical.clone();
    stage.stage = kiteframe_contract::DiagnosticStage::Admit;
    let mut retry = canonical;
    retry.retry = RetryClass::Never;
    vec![
        ("code", code),
        ("category", category),
        ("severity", severity),
        ("stage", stage),
        ("retry", retry),
    ]
}

fn capability_identity() -> CapabilityIdentity {
    capability_identity_with_name("cases.comment")
}

fn capability_identity_with_name(name: &str) -> CapabilityIdentity {
    CapabilityIdentity::try_new(
        CapabilityName::new(name).unwrap(),
        CapabilityReleaseVersion::new("1.0.0").unwrap(),
    )
    .unwrap()
}

fn resolved_requirement() -> ResolvedCapabilityRequirement {
    ResolvedCapabilityRequirement::try_new(
        {
            let descriptor = read_only_descriptor();
            LockedCapability::try_new(
                capability_identity(),
                descriptor.clone(),
                *descriptor.descriptor_digest(),
                digest(5),
                digest(6),
                digest(7),
                digest(8),
            )
            .unwrap()
        },
        true,
        vec![String::from("tenant:t1/case:case-1")],
    )
    .unwrap()
}

fn valid_traceparent() -> String {
    String::from("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
}

fn valid_admission_request() -> AdmissionRequest {
    AdmissionRequest::try_new(AdmissionRequestParts {
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        portable_digest: digest(1),
        lock_digest: digest(2),
        resolved_digest: digest(3),
        required_capabilities: vec![
            RequestedCapability::try_new(
                capability_identity(),
                vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
            )
            .unwrap(),
        ],
        optional_capabilities: Vec::new(),
        resolved_requirements: vec![resolved_requirement()],
        delegation_ancestry: DelegationAncestry::default(),
        contextual_facts: BTreeMap::new(),
        trace_context: TraceContext::try_new(valid_traceparent(), None, BTreeMap::new()).unwrap(),
    })
    .unwrap()
}

fn diagnostic_with_retry(retry: &str) -> serde_json::Value {
    json!({
        "code": "KF-CAP-003",
        "category": "capability",
        "severity": "error",
        "stage": "invoke",
        "package_path": null,
        "source_range": null,
        "message": "status is required",
        "help": null,
        "retry": retry,
        "details": {}
    })
}

fn grant_set_parts() -> CapabilityGrantSetParts {
    CapabilityGrantSetParts {
        admission_id: AdmissionId::new("adm-1").unwrap(),
        actor: ActorRef::new("actor:alice").unwrap(),
        agent: AgentRef::new("agent:case-worker").unwrap(),
        task: TaskRef::new("task:triage").unwrap(),
        session: SessionRef::new("session:1").unwrap(),
        policy_revision: PolicyRevision::new("policy:7").unwrap(),
        catalog_digest: digest(1),
        issued_at: Timestamp::new(100),
        expires_at: Timestamp::new(200),
        grants: vec![
            CapabilityGrant::try_new(CapabilityGrantParts {
                capability: capability_identity(),
                resources: vec![NormalizedResourceSelector::new("tenant:t1/case:case-1").unwrap()],
            })
            .unwrap(),
        ],
    }
}

fn digest(value: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([value; Sha256Digest::BYTE_LENGTH])
}
