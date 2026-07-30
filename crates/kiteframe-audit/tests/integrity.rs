use std::{collections::BTreeMap, fs, sync::Arc};

use kiteframe_audit::FileAuditLedger;
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CatalogIdentity, EffectClassification, EvidenceReferences, IdempotencyKey, InvocationId,
    NormalizedResourceSelector, SessionRef, Sha256Digest, TaskRef, Timestamp,
};
use kiteframe_provider::{
    AuditRecord, AuditSink, AuthorizationAuditRecord, DecisionRef, HumanPrincipalRef,
    OutcomeAuditKind, OutcomeAuditRecord, PreconditionRef, RunRef, SpanId, TenantRef, TraceId,
    WorkloadPrincipalRef,
};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn sequence_hash_linkage_and_restart_are_verified() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = FileAuditLedger::open(directory.path()).unwrap();
    let authorization = authorization_record("tenant-a", 1);

    let first = ledger
        .append(AuditRecord::Authorization(authorization))
        .await
        .unwrap();
    let second = ledger
        .append(AuditRecord::Outcome(outcome_record(
            "tenant-a",
            first.record_id(),
            2,
        )))
        .await
        .unwrap();

    assert_eq!(first.sequence(), 1);
    assert_eq!(
        first.record_hash().to_string(),
        "d042ed0bd0c985acd7bdd2199fe5d058e859a7392e30e936807d609f6ca0e05b"
    );
    assert_eq!(second.sequence(), 2);
    assert_eq!(second.previous_hash(), first.record_hash());
    let reopened = FileAuditLedger::open(directory.path()).unwrap();
    let verified = reopened.verify_partition("tenant-a").unwrap();
    assert_eq!(verified.len(), 2);
    assert_eq!(verified[0].receipt(), &first);
    assert_eq!(verified[1].receipt(), &second);
    let third = reopened
        .append(AuditRecord::Authorization(authorization_record(
            "tenant-a", 3,
        )))
        .await
        .unwrap();
    assert_eq!(third.sequence(), 3);
    assert_eq!(third.previous_hash(), second.record_hash());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_partitions_keep_independent_complete_chains() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = Arc::new(FileAuditLedger::open(directory.path()).unwrap());
    let mut tasks = Vec::new();
    for partition in ["tenant-a", "tenant-b"] {
        for sequence in 1..=12 {
            let ledger = ledger.clone();
            tasks.push(tokio::spawn(async move {
                ledger
                    .append(AuditRecord::Authorization(authorization_record(
                        partition, sequence,
                    )))
                    .await
                    .unwrap()
            }));
        }
    }
    for task in tasks {
        task.await.unwrap();
    }

    for partition in ["tenant-a", "tenant-b"] {
        let verified = ledger.verify_partition(partition).unwrap();
        assert_eq!(verified.len(), 12);
        assert_eq!(verified[0].receipt().sequence(), 1);
        assert_eq!(verified[11].receipt().sequence(), 12);
        for pair in verified.windows(2) {
            assert_eq!(
                pair[1].receipt().previous_hash(),
                pair[0].receipt().record_hash()
            );
        }
    }
}

#[tokio::test]
async fn tampering_is_detected_before_another_receipt_is_issued() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = FileAuditLedger::open(directory.path()).unwrap();
    ledger
        .append(AuditRecord::Authorization(authorization_record(
            "tenant-a", 1,
        )))
        .await
        .unwrap();
    let path = ledger.partition_path("tenant-a").unwrap();
    let original = fs::read_to_string(&path).unwrap();
    fs::write(&path, original.replace("decision-1", "decision-x")).unwrap();

    let verify_error = ledger.verify_partition("tenant-a").unwrap_err();
    assert_eq!(verify_error.code.as_str(), "KF-AUDIT-001");
    let append_error = ledger
        .append(AuditRecord::Authorization(authorization_record(
            "tenant-a", 2,
        )))
        .await
        .unwrap_err();
    assert_eq!(append_error.code.as_str(), "KF-AUDIT-001");
}

#[tokio::test]
async fn incomplete_jsonl_tail_is_rejected_before_restart_append() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = FileAuditLedger::open(directory.path()).unwrap();
    ledger
        .append(AuditRecord::Authorization(authorization_record(
            "tenant-a", 1,
        )))
        .await
        .unwrap();
    let path = ledger.partition_path("tenant-a").unwrap();
    let mut bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    fs::write(&path, bytes).unwrap();

    let reopened = FileAuditLedger::open(directory.path()).unwrap();
    let error = reopened.verify_partition("tenant-a").unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUDIT-001");
    assert!(
        reopened
            .append(AuditRecord::Authorization(authorization_record(
                "tenant-a", 2,
            )))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn records_link_complete_safe_correlation_without_sensitive_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = FileAuditLedger::open(directory.path()).unwrap();
    let authorization = ledger
        .append(AuditRecord::Authorization(authorization_record(
            "tenant-a", 1,
        )))
        .await
        .unwrap();
    ledger
        .append(AuditRecord::Outcome(outcome_record(
            "tenant-a",
            authorization.record_id(),
            2,
        )))
        .await
        .unwrap();

    let entries = ledger.verify_partition("tenant-a").unwrap();
    let authorization_json = entries[0].record();
    let outcome_json = entries[1].record();
    assert_eq!(authorization_json["record"]["humanPrincipalRef"], "human-7");
    assert_eq!(
        authorization_json["record"]["workloadPrincipalRef"],
        "workload-2"
    );
    assert_eq!(
        authorization_json["record"]["authorityRevisionDigest"],
        digest(15).to_string()
    );
    assert_eq!(
        authorization_json["record"]["traceId"],
        "0123456789abcdef0123456789abcdef"
    );
    assert_eq!(
        outcome_json["record"]["writeAheadRecordId"],
        authorization.record_id()
    );
    assert_eq!(
        outcome_json["record"]["proposalDigest"],
        authorization_json["record"]["proposalDigest"]
    );
    let persisted = fs::read_to_string(ledger.partition_path("tenant-a").unwrap()).unwrap();
    for forbidden in [
        "credential",
        "bearer",
        "rawClaims",
        "evidenceBody",
        "providerAcl",
        "arguments",
        "secret",
        "legacyDto",
    ] {
        assert!(
            !persisted
                .to_ascii_lowercase()
                .contains(&forbidden.to_ascii_lowercase()),
            "audit ledger persisted forbidden field {forbidden}"
        );
    }
}

#[tokio::test]
async fn outcome_requires_exact_same_partition_authorization_correlation() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = FileAuditLedger::open(directory.path()).unwrap();
    let authorization = ledger
        .append(AuditRecord::Authorization(authorization_record(
            "tenant-a", 1,
        )))
        .await
        .unwrap();

    macro_rules! mismatch {
        ($field:ident, $value:expr) => {{
            let mut record = outcome_record("tenant-a", authorization.record_id(), 2);
            record.$field = $value;
            (stringify!($field), record)
        }};
    }

    let mismatches = vec![
        mismatch!(
            write_ahead_record_id,
            "audit://tenant-a/99/missing".to_owned()
        ),
        mismatch!(tenant_ref, TenantRef::new("tenant-b").unwrap()),
        mismatch!(
            human_principal_ref,
            HumanPrincipalRef::new("human-other").unwrap()
        ),
        mismatch!(
            workload_principal_ref,
            WorkloadPrincipalRef::new("workload-other").unwrap()
        ),
        mismatch!(run_ref, RunRef::new("run-other").unwrap()),
        mismatch!(actor, ActorRef::new("actor-other").unwrap()),
        mismatch!(agent, AgentRef::new("agent-other").unwrap()),
        mismatch!(task, TaskRef::new("task-other").unwrap()),
        mismatch!(session, SessionRef::new("session-other").unwrap()),
        mismatch!(
            capability,
            CapabilityIdentity::try_new(
                CapabilityName::new("cases.other").unwrap(),
                CapabilityReleaseVersion::new("1.0.0").unwrap(),
            )
            .unwrap()
        ),
        mismatch!(
            resource,
            NormalizedResourceSelector::new("case:other").unwrap()
        ),
        mismatch!(admission_id, AdmissionId::new("admission-other").unwrap()),
        mismatch!(grant_digest, digest(31)),
        mismatch!(
            catalog_identity,
            CatalogIdentity {
                name: "other.catalog".to_owned(),
                revision: "1.0.0".to_owned(),
            }
        ),
        mismatch!(catalog_digest, digest(32)),
        mismatch!(descriptor_digest, digest(33)),
        mismatch!(authority_revision_digest, digest(34)),
        mismatch!(
            invocation_id,
            InvocationId::new("invocation-other").unwrap()
        ),
        mismatch!(status_id, "status://invocation-other".to_owned()),
        mismatch!(
            idempotency_key,
            IdempotencyKey::new("idempotency-other").unwrap()
        ),
        mismatch!(proposal_digest, digest(35)),
        mismatch!(portable_digest, digest(36)),
        mismatch!(lock_digest, digest(37)),
        mismatch!(binding_digest, digest(38)),
        mismatch!(resolved_digest, digest(39)),
        mismatch!(
            trace_id,
            TraceId::new("fedcba9876543210fedcba9876543210").unwrap()
        ),
        mismatch!(span_id, SpanId::new("fedcba9876543210").unwrap()),
        mismatch!(intended_effect, EffectClassification::IrreversibleWrite),
    ];

    for (field, outcome) in mismatches {
        let error = ledger
            .append(AuditRecord::Outcome(outcome))
            .await
            .expect_err(field);
        assert_eq!(error.code.as_str(), "KF-AUDIT-001", "{field}");
        assert_eq!(ledger.verify_partition("tenant-a").unwrap().len(), 1);
    }

    let receipt = ledger
        .append(AuditRecord::Outcome(outcome_record(
            "tenant-a",
            authorization.record_id(),
            2,
        )))
        .await
        .unwrap();
    assert_eq!(receipt.sequence(), 2);
    assert_eq!(ledger.verify_partition("tenant-a").unwrap().len(), 2);
}

#[tokio::test]
async fn valid_recomputed_hash_cannot_hide_outcome_correlation_tampering() {
    let directory = tempfile::tempdir().unwrap();
    let ledger = FileAuditLedger::open(directory.path()).unwrap();
    let authorization = ledger
        .append(AuditRecord::Authorization(authorization_record(
            "tenant-a", 1,
        )))
        .await
        .unwrap();
    ledger
        .append(AuditRecord::Outcome(outcome_record(
            "tenant-a",
            authorization.record_id(),
            2,
        )))
        .await
        .unwrap();
    let path = ledger.partition_path("tenant-a").unwrap();
    let mut lines = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    lines[1]["record"]["record"]["traceId"] =
        serde_json::Value::String("fedcba9876543210fedcba9876543210".to_owned());
    let sequence = lines[1]["sequence"].as_u64().unwrap();
    let previous_hash: Sha256Digest =
        serde_json::from_value(lines[1]["previousHash"].clone()).unwrap();
    let canonical_record = serde_json_canonicalizer::to_vec(&lines[1]["record"]).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(b"kiteframe:audit:v1\0");
    hasher.update(b"tenant-a");
    hasher.update(sequence.to_be_bytes());
    hasher.update(previous_hash.as_bytes());
    hasher.update(canonical_record);
    lines[1]["recordHash"] =
        serde_json::Value::String(Sha256Digest::from_bytes(hasher.finalize().into()).to_string());
    let rewritten = lines
        .iter()
        .map(|line| serde_json::to_string(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(path, rewritten).unwrap();

    let error = ledger.verify_partition("tenant-a").unwrap_err();
    assert_eq!(error.code.as_str(), "KF-AUDIT-001");
}

fn authorization_record(tenant: &str, sequence: u64) -> AuthorizationAuditRecord {
    AuthorizationAuditRecord {
        tenant_ref: TenantRef::new(tenant).unwrap(),
        human_principal_ref: HumanPrincipalRef::new("human-7").unwrap(),
        workload_principal_ref: WorkloadPrincipalRef::new("workload-2").unwrap(),
        run_ref: RunRef::new("run-9").unwrap(),
        actor: ActorRef::new("actor-7").unwrap(),
        agent: AgentRef::new("agent-2").unwrap(),
        task: TaskRef::new("task-4").unwrap(),
        session: SessionRef::new("session-3").unwrap(),
        capability: capability(),
        resource: NormalizedResourceSelector::new("case:42").unwrap(),
        admission_id: AdmissionId::new("admission-5").unwrap(),
        grant_digest: digest(10),
        catalog_identity: CatalogIdentity {
            name: "provider.catalog".to_owned(),
            revision: "1.0.0".to_owned(),
        },
        catalog_digest: digest(11),
        descriptor_digest: digest(12),
        authority_revision_digest: digest(15),
        decision_reference: DecisionRef::new(format!("decision-{sequence}")).unwrap(),
        invocation_id: InvocationId::new(format!("invocation-{sequence}")).unwrap(),
        status_id: format!("status://invocation-{sequence}"),
        idempotency_key: IdempotencyKey::new(format!("idempotency-{sequence}")).unwrap(),
        precondition_refs: vec![PreconditionRef::new("etag").unwrap()],
        evidence_refs: EvidenceReferences::try_new(BTreeMap::from([(
            "approval".to_owned(),
            serde_json::Value::String("vault://approval/7".to_owned()),
        )]))
        .unwrap(),
        proposal_digest: digest(16),
        portable_digest: digest(17),
        lock_digest: digest(18),
        binding_digest: digest(19),
        resolved_digest: digest(20),
        trace_id: TraceId::new("0123456789abcdef0123456789abcdef").unwrap(),
        span_id: SpanId::new("0123456789abcdef").unwrap(),
        intended_effect: EffectClassification::ReversibleWrite,
        timestamp: Timestamp::new(200 + sequence),
    }
}

fn outcome_record(tenant: &str, write_ahead_record_id: &str, sequence: u64) -> OutcomeAuditRecord {
    OutcomeAuditRecord {
        write_ahead_record_id: write_ahead_record_id.to_owned(),
        outcome: OutcomeAuditKind::Completion,
        tenant_ref: TenantRef::new(tenant).unwrap(),
        human_principal_ref: HumanPrincipalRef::new("human-7").unwrap(),
        workload_principal_ref: WorkloadPrincipalRef::new("workload-2").unwrap(),
        run_ref: RunRef::new("run-9").unwrap(),
        actor: ActorRef::new("actor-7").unwrap(),
        agent: AgentRef::new("agent-2").unwrap(),
        task: TaskRef::new("task-4").unwrap(),
        session: SessionRef::new("session-3").unwrap(),
        capability: capability(),
        resource: NormalizedResourceSelector::new("case:42").unwrap(),
        admission_id: AdmissionId::new("admission-5").unwrap(),
        grant_digest: digest(10),
        catalog_identity: CatalogIdentity {
            name: "provider.catalog".to_owned(),
            revision: "1.0.0".to_owned(),
        },
        catalog_digest: digest(11),
        descriptor_digest: digest(12),
        authority_revision_digest: digest(15),
        invocation_id: InvocationId::new("invocation-1").unwrap(),
        status_id: "status://invocation-1".to_owned(),
        idempotency_key: IdempotencyKey::new("idempotency-1").unwrap(),
        proposal_digest: digest(16),
        portable_digest: digest(17),
        lock_digest: digest(18),
        binding_digest: digest(19),
        resolved_digest: digest(20),
        trace_id: TraceId::new("0123456789abcdef0123456789abcdef").unwrap(),
        span_id: SpanId::new("0123456789abcdef").unwrap(),
        intended_effect: EffectClassification::ReversibleWrite,
        safe_result: None,
        safe_error: None,
        timestamp: Timestamp::new(200 + sequence),
    }
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
