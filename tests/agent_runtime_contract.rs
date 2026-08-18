use kiteframe::dsl::DefinitionKind;
use kiteframe::{
    AcceptedUserTask, AgentExecutionView, AgentRuntimeError, BlockGoal, CompleteGoal, GoalPhase,
    GoalProjection, KernelCommand, KernelCommandPayload, OperationalStatus, PrincipalId,
    PublishedDefinition, RequestContext, RuntimeCeilings, RuntimeCommandKey, RuntimeDisposition,
    RuntimeEvent, RuntimeStateSnapshot, RuntimeTransition, SubmitUserTask, TaskLifecycle,
    UserTaskId, validate_transition,
};
use serde_json::json;
use uuid::Uuid;

fn execution() -> AgentExecutionView {
    let submission: SubmitUserTask =
        serde_json::from_str(include_str!("fixtures/task/babysit_pr.submit.json")).unwrap();
    let goal_digest = submission.goal.digest().unwrap();
    AgentExecutionView {
        task: AcceptedUserTask {
            id: UserTaskId::new(Uuid::from_u128(1)),
            session_id: submission.session_id,
            principal: PrincipalId::new("test-principal").unwrap(),
            definition: PublishedDefinition {
                id: Uuid::from_u128(2),
                kind: DefinitionKind::Agent,
                name: submission.agent.name,
                version: submission.agent.version.unwrap(),
                definition_hash: "definition-digest".into(),
            },
            target: submission.target,
            preferences: submission.preferences,
            goal: submission.goal,
            goal_digest,
            revision: 4,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        },
        lifecycle: TaskLifecycle::Running,
        operational_status: OperationalStatus::Runnable,
        task_revision: 4,
        goal_phase: GoalPhase::Active,
        goal_revision: 7,
        blocker: None,
        runtime: None,
    }
}

fn command(key: &str, payload: KernelCommandPayload) -> KernelCommand {
    KernelCommand {
        schema_version: 1,
        key: RuntimeCommandKey::new(key).unwrap(),
        payload,
    }
}

fn transition(disposition: RuntimeDisposition, commands: Vec<KernelCommand>) -> RuntimeTransition {
    RuntimeTransition {
        idempotency_key: "runtime-transition-1".into(),
        expected_task_revision: 4,
        expected_goal_revision: 7,
        snapshot: RuntimeStateSnapshot::new(
            disposition,
            json!({"frames": []}),
            &Default::default(),
        )
        .unwrap(),
        commands,
    }
}

#[test]
fn snapshot_digest_is_canonical_and_ceiling_bound() {
    let ceilings = RuntimeCeilings::default();
    let first = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        json!({"frames": [], "nextFrame": 1}),
        &ceilings,
    )
    .unwrap();
    let second = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        json!({"nextFrame": 1, "frames": []}),
        &ceilings,
    )
    .unwrap();
    assert_eq!(
        first.digest(&ceilings).unwrap(),
        second.digest(&ceilings).unwrap()
    );
    assert_eq!(first.canonical_byte_count, second.canonical_byte_count);
    assert_eq!(first.ceiling_bytes, 262_144);

    let tiny = RuntimeCeilings {
        max_snapshot_bytes: 8,
        ..ceilings
    };
    assert_eq!(
        first.digest(&tiny),
        Err(AgentRuntimeError::CeilingExceeded("snapshot_bytes")),
    );
}

#[test]
fn command_payload_is_closed_and_version_one() {
    let value = json!({
        "schemaVersion": 1,
        "key": "r1-f0-harness",
        "payload": {
            "type": "fail_task",
            "payload": {"code": "no_path", "message": "stopped"}
        }
    });
    let command: KernelCommand = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(command.payload, KernelCommandPayload::FailTask(_)));
    let mut unknown = value;
    unknown["extra"] = json!(true);
    assert!(serde_json::from_value::<KernelCommand>(unknown).is_err());
    assert!(RuntimeCommandKey::new("").is_err());
}

#[test]
fn snapshot_reads_reject_tampered_envelopes_and_invalid_ceilings() {
    let ceilings = RuntimeCeilings::default();
    let snapshot = RuntimeStateSnapshot::new(
        RuntimeDisposition::Returned,
        json!({"frames": [{"index": 1}]}),
        &ceilings,
    )
    .unwrap();

    let mut unsupported = snapshot.clone();
    unsupported.schema_version = 2;
    assert!(matches!(
        unsupported.canonical_bytes(&ceilings),
        Err(AgentRuntimeError::InvalidState(_))
    ));
    let mut unsupported = snapshot.clone();
    unsupported.runtime_id = "other.v1".into();
    assert!(matches!(
        unsupported.canonical_bytes(&ceilings),
        Err(AgentRuntimeError::InvalidState(_))
    ));
    let mut scalar = snapshot.clone();
    scalar.payload = json!("not-an-object");
    assert!(matches!(
        scalar.canonical_bytes(&ceilings),
        Err(AgentRuntimeError::InvalidState(_))
    ));
    for applied_ceiling in [0, 262_145] {
        let mut invalid = snapshot.clone();
        invalid.ceiling_bytes = applied_ceiling;
        assert!(matches!(
            invalid.canonical_bytes(&ceilings),
            Err(AgentRuntimeError::InvalidState(_))
        ));
    }
    let mut tampered = snapshot.clone();
    tampered.payload["frames"][0]["index"] = json!(2);
    assert!(matches!(
        tampered.canonical_bytes(&ceilings),
        Err(AgentRuntimeError::InvalidState(_))
    ));

    for invalid in [
        RuntimeCeilings {
            max_snapshot_bytes: 0,
            ..ceilings
        },
        RuntimeCeilings {
            max_command_bytes: 65_537,
            ..ceilings
        },
        RuntimeCeilings {
            max_commands_per_transition: 0,
            ..ceilings
        },
        RuntimeCeilings {
            max_frames: 257,
            ..ceilings
        },
        RuntimeCeilings {
            max_history_items_per_frame: 0,
            ..ceilings
        },
    ] {
        assert_eq!(
            invalid.validate(),
            Err(AgentRuntimeError::CeilingExceeded("configured_ceilings"))
        );
    }
}

#[test]
fn runtime_command_keys_enforce_the_closed_ascii_contract() {
    assert!(RuntimeCommandKey::new("a").is_ok());
    assert!(RuntimeCommandKey::new("a-0_b.c/d:e").is_ok());
    assert!(RuntimeCommandKey::new("a".repeat(128)).is_ok());
    for invalid in [
        "".to_owned(),
        "A".to_owned(),
        "white space".to_owned(),
        "é".to_owned(),
        "a".repeat(129),
    ] {
        assert!(RuntimeCommandKey::new(invalid).is_err());
    }
}

#[test]
fn transition_validation_enforces_revisions_keys_ceilings_and_dispositions() {
    let execution = execution();
    let ceilings = RuntimeCeilings::default();
    let context = || {
        command(
            "context",
            KernelCommandPayload::RequestContext(RequestContext {
                evidence_type: "github.context".into(),
                subject: json!({"pullRequest": 142}),
            }),
        )
    };
    assert_eq!(
        validate_transition(
            &transition(RuntimeDisposition::Running, vec![context()]),
            &execution,
            &ceilings,
        ),
        Ok(())
    );
    assert_eq!(
        validate_transition(
            &transition(RuntimeDisposition::Running, Vec::new()),
            &execution,
            &ceilings,
        ),
        Ok(())
    );
    assert_eq!(
        validate_transition(
            &transition(RuntimeDisposition::Returned, Vec::new()),
            &execution,
            &ceilings,
        ),
        Ok(())
    );
    assert_eq!(
        validate_transition(
            &transition(
                RuntimeDisposition::Blocked,
                vec![command(
                    "blocked",
                    KernelCommandPayload::BlockGoal(BlockGoal {
                        blocked_when_index: 0,
                        reason_code: "actor_unavailable".into(),
                        explanation: "The actor is unavailable.".into(),
                        evidence_ids: Vec::new(),
                        unlock_conditions: vec!["The actor responds.".into()],
                    }),
                )],
            ),
            &execution,
            &ceilings,
        ),
        Ok(())
    );
    assert_eq!(
        validate_transition(
            &transition(
                RuntimeDisposition::Terminating,
                vec![command(
                    "complete",
                    KernelCommandPayload::CompleteGoal(CompleteGoal {
                        assertions: Vec::new(),
                    }),
                )],
            ),
            &execution,
            &ceilings,
        ),
        Ok(())
    );

    let mut wrong_task_revision = transition(RuntimeDisposition::Running, vec![context()]);
    wrong_task_revision.expected_task_revision = 5;
    assert!(matches!(
        validate_transition(&wrong_task_revision, &execution, &ceilings),
        Err(AgentRuntimeError::InvalidTransition(_))
    ));
    let mut wrong_goal_revision = transition(RuntimeDisposition::Running, vec![context()]);
    wrong_goal_revision.expected_goal_revision = 8;
    assert!(matches!(
        validate_transition(&wrong_goal_revision, &execution, &ceilings),
        Err(AgentRuntimeError::InvalidTransition(_))
    ));
    for invalid_key in ["".to_owned(), "x".repeat(129)] {
        let mut invalid = transition(RuntimeDisposition::Running, vec![context()]);
        invalid.idempotency_key = invalid_key;
        assert!(matches!(
            validate_transition(&invalid, &execution, &ceilings),
            Err(AgentRuntimeError::InvalidTransition(_))
        ));
    }
    let mut wrong_schema = transition(RuntimeDisposition::Running, vec![context()]);
    wrong_schema.commands[0].schema_version = 2;
    assert!(matches!(
        validate_transition(&wrong_schema, &execution, &ceilings),
        Err(AgentRuntimeError::InvalidTransition(_))
    ));
    let duplicated = transition(RuntimeDisposition::Running, vec![context(), context()]);
    assert!(matches!(
        validate_transition(&duplicated, &execution, &ceilings),
        Err(AgentRuntimeError::InvalidTransition(_))
    ));

    for invalid in [
        transition(RuntimeDisposition::Returned, vec![context()]),
        transition(RuntimeDisposition::Blocked, vec![context()]),
        transition(RuntimeDisposition::Terminating, vec![context()]),
    ] {
        assert!(matches!(
            validate_transition(&invalid, &execution, &ceilings),
            Err(AgentRuntimeError::InvalidTransition(_))
        ));
    }

    let small_command_ceiling = RuntimeCeilings {
        max_command_bytes: 1,
        ..ceilings
    };
    assert_eq!(
        validate_transition(
            &transition(RuntimeDisposition::Running, vec![context()]),
            &execution,
            &small_command_ceiling,
        ),
        Err(AgentRuntimeError::CeilingExceeded("command_bytes"))
    );
}

#[test]
fn transition_validation_rejects_a_deserialized_invalid_command_key() {
    let mut value = serde_json::to_value(transition(
        RuntimeDisposition::Running,
        vec![command(
            "valid",
            KernelCommandPayload::RequestContext(RequestContext {
                evidence_type: "github.context".into(),
                subject: json!({}),
            }),
        )],
    ))
    .unwrap();
    value["commands"][0]["key"] = json!("INVALID");
    let invalid: RuntimeTransition = serde_json::from_value(value).unwrap();

    assert!(matches!(
        validate_transition(&invalid, &execution(), &RuntimeCeilings::default()),
        Err(AgentRuntimeError::InvalidTransition(_))
    ));
}

#[test]
fn goal_projection_and_runtime_event_accessors_are_closed_projections() {
    let execution = execution();
    let projection = GoalProjection::from_contract(&execution.task.goal);
    assert_eq!(projection.objective, execution.task.goal.objective);
    assert_eq!(projection.success.len(), 3);
    assert_eq!(projection.success[0].id, "reviews_resolved");
    assert_eq!(
        projection.success[0].evidence_type,
        "github.unresolved_reviews"
    );
    assert_eq!(projection.constraints, execution.task.goal.constraints);

    let resumed = RuntimeEvent::PrincipalResumed {
        event_id: "event-1".into(),
    };
    assert_eq!(resumed.event_id(), "event-1");
    assert_eq!(resumed.command_key(), None);
}
