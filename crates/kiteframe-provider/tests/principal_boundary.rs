use kiteframe_contract::{ActorRef, AdmissionId, AgentRef, SessionRef, TaskRef, Timestamp};
use kiteframe_provider::{
    PortableInvocationRefs, VerifiedHumanPrincipal, VerifiedWorkloadPrincipal, correlate_principals,
};

#[test]
fn independently_verified_human_and_workload_are_both_required() {
    let context = correlate_principals(
        verified_human("tenant-1", "human-7", "actor-7", 500),
        verified_workload(
            "tenant-1",
            "harness-2",
            "run-9",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
            450,
        ),
        portable_refs(
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
            100,
        ),
    )
    .unwrap();

    assert_eq!(context.tenant_ref().as_str(), "tenant-1");
    assert_eq!(context.human_ref().as_str(), "human-7");
    assert_eq!(context.workload_ref().as_str(), "harness-2");
    assert_eq!(context.run_ref().as_str(), "run-9");
    assert_eq!(context.actor_ref().as_str(), "actor-7");
    assert_eq!(context.agent_ref().as_str(), "agent-2");
    assert_eq!(context.task_ref().as_str(), "task-4");
    assert_eq!(context.session_ref().as_str(), "session-3");
    assert_eq!(context.admission_ref().as_str(), "admission-5");
    assert_eq!(context.expires_at(), Timestamp::new(450));
}

#[test]
fn verified_tenant_or_subject_mismatch_fails_closed() {
    let cases = [
        (
            verified_human("tenant-1", "human-7", "actor-7", 500),
            verified_workload(
                "tenant-2",
                "harness-2",
                "run-9",
                "agent-2",
                "task-4",
                "session-3",
                "admission-5",
                450,
            ),
            portable_refs(
                "actor-7",
                "agent-2",
                "task-4",
                "session-3",
                "admission-5",
                100,
            ),
        ),
        (
            verified_human("tenant-1", "human-7", "actor-other", 500),
            verified_workload(
                "tenant-1",
                "harness-2",
                "run-9",
                "agent-2",
                "task-4",
                "session-3",
                "admission-5",
                450,
            ),
            portable_refs(
                "actor-7",
                "agent-2",
                "task-4",
                "session-3",
                "admission-5",
                100,
            ),
        ),
        (
            verified_human("tenant-1", "human-7", "actor-7", 500),
            verified_workload(
                "tenant-1",
                "harness-2",
                "run-9",
                "agent-other",
                "task-4",
                "session-3",
                "admission-5",
                450,
            ),
            portable_refs(
                "actor-7",
                "agent-2",
                "task-4",
                "session-3",
                "admission-5",
                100,
            ),
        ),
        (
            verified_human("tenant-1", "human-7", "actor-7", 500),
            verified_workload(
                "tenant-1",
                "harness-2",
                "run-9",
                "agent-2",
                "task-other",
                "session-3",
                "admission-5",
                450,
            ),
            portable_refs(
                "actor-7",
                "agent-2",
                "task-4",
                "session-3",
                "admission-5",
                100,
            ),
        ),
    ];

    for (human, workload, refs) in cases {
        let error = correlate_principals(human, workload, refs).unwrap_err();
        assert_eq!(error.code.as_str(), "KF-AUTH-003");
    }
}

#[test]
fn expired_or_wrong_session_and_admission_bindings_fail_closed() {
    let human = verified_human("tenant-1", "human-7", "actor-7", 100);
    let workload = verified_workload(
        "tenant-1",
        "harness-2",
        "run-9",
        "agent-2",
        "task-4",
        "session-3",
        "admission-5",
        450,
    );
    let expired = correlate_principals(
        human,
        workload,
        portable_refs(
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
            100,
        ),
    )
    .unwrap_err();
    assert_eq!(expired.code.as_str(), "KF-AUTH-003");

    let wrong_binding = correlate_principals(
        verified_human("tenant-1", "human-7", "actor-7", 500),
        verified_workload(
            "tenant-1",
            "harness-2",
            "run-9",
            "agent-2",
            "task-4",
            "session-other",
            "admission-5",
            450,
        ),
        portable_refs(
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
            100,
        ),
    )
    .unwrap_err();
    assert_eq!(wrong_binding.code.as_str(), "KF-AUTH-003");

    let wrong_admission = correlate_principals(
        verified_human("tenant-1", "human-7", "actor-7", 500),
        verified_workload(
            "tenant-1",
            "harness-2",
            "run-9",
            "agent-2",
            "task-4",
            "session-3",
            "admission-other",
            450,
        ),
        portable_refs(
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
            100,
        ),
    )
    .unwrap_err();
    assert_eq!(wrong_admission.code.as_str(), "KF-AUTH-003");

    let expired_workload = correlate_principals(
        verified_human("tenant-1", "human-7", "actor-7", 500),
        verified_workload(
            "tenant-1",
            "harness-2",
            "run-9",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
            100,
        ),
        portable_refs(
            "actor-7",
            "agent-2",
            "task-4",
            "session-3",
            "admission-5",
            100,
        ),
    )
    .unwrap_err();
    assert_eq!(expired_workload.code.as_str(), "KF-AUTH-003");
}

fn verified_human(
    tenant: &str,
    human: &str,
    actor: &str,
    expires_at: u64,
) -> VerifiedHumanPrincipal {
    VerifiedHumanPrincipal::try_new(
        tenant,
        human,
        ActorRef::new(actor).unwrap(),
        Timestamp::new(expires_at),
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn verified_workload(
    tenant: &str,
    workload: &str,
    run: &str,
    agent: &str,
    task: &str,
    session: &str,
    admission: &str,
    expires_at: u64,
) -> VerifiedWorkloadPrincipal {
    VerifiedWorkloadPrincipal::try_new(
        tenant,
        workload,
        run,
        AgentRef::new(agent).unwrap(),
        TaskRef::new(task).unwrap(),
        SessionRef::new(session).unwrap(),
        AdmissionId::new(admission).unwrap(),
        Timestamp::new(expires_at),
    )
    .unwrap()
}

fn portable_refs(
    actor: &str,
    agent: &str,
    task: &str,
    session: &str,
    admission: &str,
    correlated_at: u64,
) -> PortableInvocationRefs {
    PortableInvocationRefs::new(
        ActorRef::new(actor).unwrap(),
        AgentRef::new(agent).unwrap(),
        TaskRef::new(task).unwrap(),
        SessionRef::new(session).unwrap(),
        AdmissionId::new(admission).unwrap(),
        Timestamp::new(correlated_at),
    )
}
