use std::fs;

use kiteframe::goal::{GoalContract, GoalContractError};
use kiteframe::task::{HarnessRole, SubmitUserTask, TaskTarget, UserTaskError};
use kiteframe::{
    AgentCommandId, CancelUserTask, CriterionAssertion, EvidenceId, EvidenceRecord,
    EvidenceSourceIdentity, GoalBlocker, GoalPhase, IdempotencyKey, OperationalStatus,
    PauseUserTask, ResumeUserTask, SessionId, StartAgent, TaskLifecycle, UserTaskId,
};
use uuid::Uuid;

fn valid_request() -> SubmitUserTask {
    serde_json::from_str(include_str!("fixtures/task/babysit_pr.submit.json")).unwrap()
}

fn assert_goal_error(mutate: impl FnOnce(&mut GoalContract), expected: GoalContractError) {
    let mut goal = valid_request().goal;
    mutate(&mut goal);
    assert_eq!(goal.validate(), Err(expected));
}

fn assert_goal_digest_changes(mutate: impl FnOnce(&mut GoalContract)) {
    let mut changed = valid_request().goal;
    let original = changed.digest().unwrap();
    mutate(&mut changed);
    assert_ne!(changed.digest().unwrap(), original);
}

fn assert_request_digest_changes(mutate: impl FnOnce(&mut SubmitUserTask)) {
    let mut changed = valid_request();
    let original = changed.request_digest().unwrap();
    mutate(&mut changed);
    assert_ne!(changed.request_digest().unwrap(), original);
}

#[test]
fn reviewed_user_task_contract_is_checked_in() {
    let reference = fs::read_to_string("docs/reference/user-task-and-goal-contract.md")
        .expect("task and goal reference exists");
    let fixture = fs::read_to_string("tests/fixtures/task/babysit_pr.submit.json")
        .expect("canonical submission fixture exists");

    assert!(reference.contains("GoalContract is immutable"));
    assert!(reference.contains("Slice 2 owns goal phases"));
    assert!(
        reference.contains("| `CompleteGoal` accepted | `succeeded` | `finished` | `complete` |")
    );
    assert!(reference.contains("A `HarnessResult` supplies an invocation outcome only"));
    let value: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    assert_eq!(value["target"]["type"], "github_pull_request");
    assert_eq!(value["goal"]["success"].as_array().unwrap().len(), 3);
    assert!(value.get("principal").is_none());
}

#[test]
fn canonical_fixture_validates_and_has_order_stable_digests() {
    let first = valid_request();
    first.validate().unwrap();

    let reordered: SubmitUserTask = serde_json::from_value(serde_json::json!({
        "idempotencyKey": "submit-babysit-pr-142",
        "goal": first.goal.clone(),
        "preferences": first.preferences.clone(),
        "target": first.target.clone(),
        "agent": first.agent.clone(),
        "sessionId": first.session_id.clone(),
    }))
    .unwrap();

    assert_eq!(
        first.request_digest().unwrap(),
        reordered.request_digest().unwrap()
    );
    assert_eq!(
        first.goal.digest().unwrap(),
        reordered.goal.digest().unwrap()
    );
}

#[test]
fn rejects_every_goal_boundary_with_the_typed_error() {
    for version in [0, 2] {
        assert_goal_error(
            |goal| goal.version = version,
            GoalContractError::UnsupportedVersion,
        );
    }
    assert_goal_error(
        |goal| goal.objective = " ".into(),
        GoalContractError::InvalidObjective,
    );
    assert_goal_error(
        |goal| goal.objective = "x".repeat(4_097),
        GoalContractError::InvalidObjective,
    );
    assert_goal_error(
        |goal| goal.success.clear(),
        GoalContractError::InvalidSuccessCount,
    );
    assert_goal_error(
        |goal| goal.success = vec![goal.success[0].clone(); 33],
        GoalContractError::InvalidSuccessCount,
    );
    assert_goal_error(
        |goal| goal.success[0].id = "Bad-ID".into(),
        GoalContractError::InvalidCriterionId("Bad-ID".into()),
    );
    assert_goal_error(
        |goal| goal.success[0].id = "a".repeat(65),
        GoalContractError::InvalidCriterionId("a".repeat(65)),
    );
    assert_goal_error(
        |goal| goal.success[1].id = goal.success[0].id.clone(),
        GoalContractError::DuplicateCriterionId("reviews_resolved".into()),
    );
    assert_goal_error(
        |goal| goal.success[0].statement = " ".into(),
        GoalContractError::InvalidCriterionStatement,
    );
    assert_goal_error(
        |goal| goal.success[0].statement = "x".repeat(2_049),
        GoalContractError::InvalidCriterionStatement,
    );
    assert_goal_error(
        |goal| goal.success[0].evidence = "invalid".into(),
        GoalContractError::InvalidEvidenceType("invalid".into()),
    );
    assert_goal_error(
        |goal| goal.success[0].evidence = "github.Invalid".into(),
        GoalContractError::InvalidEvidenceType("github.Invalid".into()),
    );
    let long_evidence = format!("a.{}", "b".repeat(127));
    assert_goal_error(
        |goal| goal.success[0].evidence = long_evidence.clone(),
        GoalContractError::InvalidEvidenceType(long_evidence.clone()),
    );
    assert_goal_error(
        |goal| goal.constraints = vec!["valid".into(); 33],
        GoalContractError::InvalidConstraints,
    );
    assert_goal_error(
        |goal| goal.constraints = vec![" ".into()],
        GoalContractError::InvalidConstraints,
    );
    assert_goal_error(
        |goal| goal.constraints = vec!["x".repeat(2_049)],
        GoalContractError::InvalidConstraints,
    );
    assert_goal_error(
        |goal| goal.blocked_when = vec!["valid".into(); 33],
        GoalContractError::InvalidBlockedConditions,
    );
    assert_goal_error(
        |goal| goal.blocked_when = vec![" ".into()],
        GoalContractError::InvalidBlockedConditions,
    );
    assert_goal_error(
        |goal| goal.blocked_when = vec!["x".repeat(2_049)],
        GoalContractError::InvalidBlockedConditions,
    );
}

#[test]
fn accepts_exact_count_and_character_boundaries() {
    let mut request = valid_request();
    request.goal.objective = "é".repeat(4_096);
    request.goal.success = (0..32)
        .map(|index| {
            let mut criterion = request.goal.success[0].clone();
            criterion.id = format!("criterion_{index}");
            criterion.statement = "é".repeat(2_048);
            criterion
        })
        .collect();
    request.goal.success[0].id = "a".repeat(64);
    request.goal.success[0].evidence = format!("a.{}", "b".repeat(126));
    request.goal.constraints = vec!["é".repeat(2_048); 32];
    request.goal.blocked_when = vec!["é".repeat(2_048); 32];
    request.agent.name = "a".repeat(256);
    request.target = TaskTarget::GithubPullRequest {
        repository: format!("{}/{}", "a".repeat(127), "b".repeat(127)),
        number: 1,
    };

    request.validate().unwrap();
}

#[test]
fn rejects_unknown_keys_at_every_request_object() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/task/babysit_pr.submit.json")).unwrap();
    for pointer in [
        "",
        "/agent",
        "/target",
        "/preferences",
        "/goal",
        "/goal/success/0",
    ] {
        let mut value = fixture.clone();
        let object = if pointer.is_empty() {
            value.as_object_mut().unwrap()
        } else {
            value.pointer_mut(pointer).unwrap().as_object_mut().unwrap()
        };
        object.insert("unknown".into(), serde_json::json!(true));
        assert!(
            serde_json::from_value::<SubmitUserTask>(value).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn rejects_invalid_task_selector_and_target() {
    let mut request = valid_request();
    request.agent.name = "".into();
    assert_eq!(request.validate(), Err(UserTaskError::InvalidAgentName));

    let mut request = valid_request();
    request.agent.name = "1agent".into();
    assert_eq!(request.validate(), Err(UserTaskError::InvalidAgentName));

    let mut request = valid_request();
    request.agent.name = "bad-agent".into();
    assert_eq!(request.validate(), Err(UserTaskError::InvalidAgentName));

    let mut request = valid_request();
    request.agent.name = "a".repeat(257);
    assert_eq!(request.validate(), Err(UserTaskError::InvalidAgentName));

    let mut request = valid_request();
    request.agent.version = Some(0);
    assert_eq!(request.validate(), Err(UserTaskError::InvalidAgentVersion));

    let mut request = valid_request();
    request.target = TaskTarget::GithubPullRequest {
        repository: "/acme/payments".into(),
        number: 142,
    };
    assert_eq!(request.validate(), Err(UserTaskError::InvalidRepository));

    let mut request = valid_request();
    request.target = TaskTarget::GithubPullRequest {
        repository: format!("{}/{}", "a".repeat(128), "b".repeat(127)),
        number: 142,
    };
    assert_eq!(request.validate(), Err(UserTaskError::InvalidRepository));

    let mut request = valid_request();
    request.target = TaskTarget::GithubPullRequest {
        repository: "acme/payments".into(),
        number: 0,
    };
    assert_eq!(
        request.validate(),
        Err(UserTaskError::InvalidPullRequestNumber)
    );
}

#[test]
fn rejects_blank_wire_idempotency_keys_at_the_task_boundary() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/task/babysit_pr.submit.json")).unwrap();

    for invalid_key in ["", " \t\n "] {
        let mut value = fixture.clone();
        value["idempotencyKey"] = serde_json::json!(invalid_key);
        let request: SubmitUserTask =
            serde_json::from_value(value).expect("wire deserialization remains transparent");

        assert_eq!(
            request.validate(),
            Err(UserTaskError::InvalidIdempotencyKey)
        );
    }
}

#[test]
fn goal_digest_binds_every_goal_field() {
    assert_goal_digest_changes(|goal| goal.version = 2);
    assert_goal_digest_changes(|goal| goal.objective.push_str(" More."));
    assert_goal_digest_changes(|goal| goal.success[0].id.push_str("_changed"));
    assert_goal_digest_changes(|goal| goal.success[0].statement.push_str(" More."));
    assert_goal_digest_changes(|goal| goal.success[0].evidence = "github.changed".into());
    assert_goal_digest_changes(|goal| goal.constraints.push("Another constraint.".into()));
    assert_goal_digest_changes(|goal| {
        goal.blocked_when.push("Another blocked condition.".into());
    });
}

#[test]
fn request_digest_excludes_only_idempotency_key() {
    let original = valid_request();
    let mut changed_key = original.clone();
    changed_key.idempotency_key = IdempotencyKey::new("another-submission-key").unwrap();
    assert_eq!(
        changed_key.request_digest().unwrap(),
        original.request_digest().unwrap()
    );

    assert_request_digest_changes(|request| {
        request.session_id = SessionId::new(Uuid::from_u128(1));
    });
    assert_request_digest_changes(|request| request.agent.name = "another_agent".into());
    assert_request_digest_changes(|request| request.agent.version = Some(2));
    assert_request_digest_changes(|request| {
        request.target = TaskTarget::GithubPullRequest {
            repository: "acme/other".into(),
            number: 142,
        };
    });
    assert_request_digest_changes(|request| {
        request.target = TaskTarget::GithubPullRequest {
            repository: "acme/payments".into(),
            number: 143,
        };
    });
    assert_request_digest_changes(|request| {
        request.preferences.harness_role = Some(HarnessRole::Reasoning);
    });
    assert_request_digest_changes(|request| request.goal.version = 2);
    assert_request_digest_changes(|request| request.goal.objective.push_str(" More."));
    assert_request_digest_changes(|request| request.goal.success[0].id.push_str("_changed"));
    assert_request_digest_changes(|request| {
        request.goal.success[0].statement.push_str(" More.");
    });
    assert_request_digest_changes(|request| {
        request.goal.success[0].evidence = "github.changed".into();
    });
    assert_request_digest_changes(|request| {
        request.goal.constraints.push("Another constraint.".into());
    });
    assert_request_digest_changes(|request| {
        request
            .goal
            .blocked_when
            .push("Another blocked condition.".into());
    });
}

#[test]
fn lifecycle_commands_and_evidence_have_closed_stable_wire_shapes() {
    assert_eq!(serde_json::to_value(GoalPhase::Active).unwrap(), "active");
    assert_eq!(
        serde_json::to_value(TaskLifecycle::Cancelled).unwrap(),
        "cancelled"
    );
    assert_eq!(
        serde_json::to_value(OperationalStatus::WaitingForHarness).unwrap(),
        "waiting_for_harness"
    );

    let session_id = SessionId::new(Uuid::from_u128(1));
    let task_id = UserTaskId::new(Uuid::from_u128(2));
    let idempotency_key = IdempotencyKey::new("command-1").unwrap();
    let start = StartAgent {
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        expected_task_revision: 3,
        expected_goal_revision: 4,
        idempotency_key: idempotency_key.clone(),
    };
    let pause = PauseUserTask {
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        expected_task_revision: 3,
        expected_goal_revision: 4,
        reason: "principal requested a pause".into(),
        idempotency_key: idempotency_key.clone(),
    };
    let resume = ResumeUserTask {
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        expected_task_revision: 3,
        expected_goal_revision: 4,
        idempotency_key: idempotency_key.clone(),
    };
    let cancel = CancelUserTask {
        session_id,
        task_id: task_id.clone(),
        expected_task_revision: 3,
        expected_goal_revision: 4,
        reason: None,
        idempotency_key,
    };
    for value in [
        serde_json::to_value(start).unwrap(),
        serde_json::to_value(pause).unwrap(),
        serde_json::to_value(resume).unwrap(),
        serde_json::to_value(cancel).unwrap(),
    ] {
        assert_eq!(value["expectedTaskRevision"], 3);
        assert_eq!(value["expectedGoalRevision"], 4);
        assert_eq!(value["idempotencyKey"], "command-1");
    }

    let evidence_id = EvidenceId::new(Uuid::from_u128(3));
    let record = EvidenceRecord {
        id: evidence_id.clone(),
        task_id,
        evidence_type: "github.required_checks".into(),
        source: EvidenceSourceIdentity::BuiltinHarness {
            command_id: AgentCommandId::new(Uuid::from_u128(4)),
        },
        digest: "evidence-digest".into(),
    };
    let wire = serde_json::to_value(&record).unwrap();
    assert_eq!(wire["evidenceType"], "github.required_checks");
    assert_eq!(wire["source"]["kind"], "builtin_harness");
    assert_eq!(
        serde_json::from_value::<EvidenceRecord>(wire.clone()).unwrap(),
        record
    );
    let mut unknown = wire;
    unknown["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<EvidenceRecord>(unknown).is_err());

    let assertion = CriterionAssertion {
        criterion_id: "checks_passed".into(),
        evidence_id: evidence_id.clone(),
    };
    assert_eq!(
        serde_json::to_value(assertion).unwrap()["criterionId"],
        "checks_passed"
    );
    let blocker = GoalBlocker {
        blocked_when_index: 0,
        reason_code: "actor_unavailable".into(),
        explanation: "The actor is unavailable.".into(),
        evidence_ids: vec![evidence_id],
        unlock_conditions: vec!["The actor responds.".into()],
        goal_revision: 5,
    };
    assert_eq!(serde_json::to_value(blocker).unwrap()["goalRevision"], 5);
}
