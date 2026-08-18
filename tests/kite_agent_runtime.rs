use std::collections::BTreeSet;

use kiteframe::dsl::{
    CompiledDefinition, CompiledType, ContextMode, DefinitionKind, Limits, ModelRole, compile,
};
use kiteframe::{
    AcceptedUserTask, AgentExecutionView, AgentRuntime, AgentRuntimeError, AgentSelector,
    CatalogTool, ClosedSchema, EffectKind, EvidenceId, GoalContract, GoalPhase, IdempotencyKey,
    InvokeHarness, KernelCommand, KernelCommandPayload, KiteAgentRuntime, KiteFrameId,
    KiteFrameState, OperationalStatus, PrincipalId, PublishedDefinition, RecoverySupport,
    RuntimeCeilings, RuntimeCommandKey, RuntimeDisposition, RuntimeEvent, RuntimeHandle,
    RuntimeHistoryItem, RuntimeHistoryKind, RuntimeStart, RuntimeStateSnapshot, SessionId,
    SubmitUserTask, SuccessCriterion, TaskLifecycle, TaskPreferences, TaskTarget,
    ToolCatalogSnapshot, UserTaskId, validate_transition,
};
use serde_json::{Value, json};
use uuid::Uuid;

struct RuntimeFixture {
    runtime: KiteAgentRuntime,
    execution: AgentExecutionView,
    definition: CompiledDefinition,
}

impl RuntimeFixture {
    fn simple(outcome_body: &str) -> Self {
        let source = format!(
            r#"agent test_agent(repository: string, pull_request: integer) -> string {{
  entry inspect(repository = repository, pull_request = pull_request)
  limits {{
    steps: 100
    model_turns_per_task: 10
    tool_calls_per_response: 4
    parallel_branches: 4
    response_item_bytes: 16384
    active_execution_seconds: 60
    elapsed_seconds: 600
  }}
  task inspect(repository: string, pull_request: integer) -> string {{
    model reasoning
    prompt """Inspect the pull request."""
    tools []
    {outcome_body}
  }}
}}"#
        );
        Self::from_source(&source, RuntimeCeilings::default())
    }

    fn from_source(source: &str, ceilings: RuntimeCeilings) -> Self {
        let catalog =
            ToolCatalogSnapshot::new(SessionId::new(Uuid::from_u128(10)), Uuid::from_u128(11), [])
                .unwrap();
        Self::from_source_and_catalog(source, ceilings, &catalog)
    }

    fn from_source_and_catalog(
        source: &str,
        ceilings: RuntimeCeilings,
        catalog: &ToolCatalogSnapshot,
    ) -> Self {
        let definition = compile(source, catalog, &Limits::default()).unwrap();
        let submission = SubmitUserTask {
            session_id: SessionId::new(Uuid::from_u128(10)),
            agent: AgentSelector {
                name: definition.name.clone(),
                version: Some(1),
            },
            target: TaskTarget::GithubPullRequest {
                repository: "acme/payments".into(),
                number: 142,
            },
            preferences: TaskPreferences { harness_role: None },
            goal: GoalContract {
                version: 1,
                objective: "Finish the pull request safely".into(),
                success: vec![SuccessCriterion {
                    id: "finished".into(),
                    statement: "The requested work is finished".into(),
                    evidence: "github.pull_request".into(),
                }],
                constraints: vec!["Do not rewrite history".into()],
                blocked_when: vec!["Required access is unavailable".into()],
            },
            idempotency_key: IdempotencyKey::new("submit-runtime-test").unwrap(),
        };
        let goal_digest = submission.goal.digest().unwrap();
        let execution = AgentExecutionView {
            task: AcceptedUserTask {
                id: UserTaskId::new(Uuid::from_u128(1)),
                session_id: submission.session_id,
                principal: PrincipalId::new("runtime-test").unwrap(),
                definition: PublishedDefinition {
                    id: Uuid::from_u128(2),
                    kind: DefinitionKind::Agent,
                    name: submission.agent.name,
                    version: 1,
                    definition_hash: definition.definition_hash.clone(),
                },
                target: submission.target,
                preferences: submission.preferences,
                goal: submission.goal,
                goal_digest,
                revision: 0,
                created_at: time::OffsetDateTime::UNIX_EPOCH,
            },
            lifecycle: TaskLifecycle::Accepted,
            operational_status: OperationalStatus::NotStarted,
            task_revision: 0,
            goal_phase: GoalPhase::Active,
            goal_revision: 0,
            blocker: None,
            runtime: None,
        };
        Self {
            runtime: KiteAgentRuntime::new(ceilings),
            execution,
            definition,
        }
    }

    fn with_tool() -> Self {
        let tool = CatalogTool::new(
            "docs/search",
            "docs",
            "Search documents.",
            ClosedSchema::object("input", Default::default()),
            ClosedSchema::object("output", Default::default()),
            BTreeSet::new(),
            EffectKind::ReadOnly,
            RecoverySupport::NotRequired,
        )
        .unwrap();
        let catalog = ToolCatalogSnapshot::new(
            SessionId::new(Uuid::from_u128(10)),
            Uuid::from_u128(11),
            [tool],
        )
        .unwrap();
        Self::from_source_and_catalog(
            r#"agent tool_agent(repository: string, pull_request: integer) -> string {
  entry inspect(repository = repository, pull_request = pull_request)
  limits { steps: 20 model_turns_per_task: 5 tool_calls_per_response: 2 parallel_branches: 2 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }
  task inspect(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Inspect.""" tools [search]
    on ok(answer: string) { done answer }
  }
}"#,
            RuntimeCeilings::default(),
            &catalog,
        )
    }

    fn history_graph() -> Self {
        Self::from_source(
            r#"agent history_agent(repository: string, pull_request: integer) -> string {
  entry root(repository = repository, pull_request = pull_request)
  limits {
    steps: 100
    model_turns_per_task: 10
    tool_calls_per_response: 4
    parallel_branches: 4
    response_item_bytes: 16384
    active_execution_seconds: 60
    elapsed_seconds: 600
  }
  task root(repository: string, pull_request: integer) -> string {
    model reasoning
    prompt """Split the work."""
    tools []
    on split(left: string, right: string) {
      fork {
        first = call branch(value = left) context isolated
        second = call branch(value = right) context kept
      } join
      done first
    }
  }
  task branch(value: string) -> string {
    model reasoning
    prompt """Inspect one branch."""
    tools []
    on ok(result: string) { done result }
  }
}"#,
            RuntimeCeilings::default(),
        )
    }

    fn start_input(&self) -> RuntimeStart<'_> {
        RuntimeStart {
            execution: &self.execution,
            definition: &self.definition,
        }
    }

    fn handle_harness(
        &self,
        snapshot: kiteframe::RuntimeStateSnapshot,
        command: &KernelCommand,
        outcome: &str,
        fields: Value,
    ) -> kiteframe::RuntimeTransition {
        self.handle_harness_at(
            snapshot,
            command,
            outcome,
            fields,
            1,
            "evt-1",
            Uuid::from_u128(20),
            vec![RuntimeHistoryItem {
                sequence: 0,
                kind: RuntimeHistoryKind::HarnessOutput,
                payload: json!("finished traversal"),
            }],
        )
        .unwrap()
    }

    fn execution_with_snapshot(
        &self,
        snapshot: RuntimeStateSnapshot,
        task_revision: u64,
        status: OperationalStatus,
    ) -> AgentExecutionView {
        let mut execution = self.execution.clone();
        execution.lifecycle = TaskLifecycle::Running;
        execution.operational_status = status;
        execution.task_revision = task_revision;
        execution.task.revision = task_revision;
        execution.runtime = Some(snapshot);
        execution
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_harness_at(
        &self,
        snapshot: RuntimeStateSnapshot,
        command: &KernelCommand,
        outcome: &str,
        fields: Value,
        task_revision: u64,
        event_id: &str,
        evidence_id: Uuid,
        history: Vec<RuntimeHistoryItem>,
    ) -> Result<kiteframe::RuntimeTransition, AgentRuntimeError> {
        let execution = self.execution_with_snapshot(
            snapshot,
            task_revision,
            OperationalStatus::WaitingForHarness,
        );
        let event = RuntimeEvent::HarnessResult {
            event_id: event_id.into(),
            command_key: command.key.clone(),
            outcome: outcome.into(),
            fields,
            evidence_id: EvidenceId::new(evidence_id),
            history,
        };
        self.runtime.handle(RuntimeHandle {
            execution: &execution,
            definition: &self.definition,
            event: &event,
        })
    }

    fn handle_event(
        &self,
        snapshot: RuntimeStateSnapshot,
        task_revision: u64,
        status: OperationalStatus,
        event: &RuntimeEvent,
    ) -> Result<kiteframe::RuntimeTransition, AgentRuntimeError> {
        let execution = self.execution_with_snapshot(snapshot, task_revision, status);
        self.runtime.handle(RuntimeHandle {
            execution: &execution,
            definition: &self.definition,
            event,
        })
    }

    fn decode(&self, snapshot: &RuntimeStateSnapshot) -> kiteframe::KiteRuntimeState {
        self.runtime.decode_snapshot(snapshot).unwrap()
    }

    fn root_history(&self) -> Vec<RuntimeHistoryItem> {
        vec![RuntimeHistoryItem {
            sequence: 0,
            kind: RuntimeHistoryKind::HarnessOutput,
            payload: json!("root result"),
        }]
    }
}

fn harness_invocations(commands: &[KernelCommand]) -> Vec<&InvokeHarness> {
    commands
        .iter()
        .filter_map(|command| match &command.payload {
            KernelCommandPayload::InvokeHarness(invocation) => Some(invocation),
            _ => None,
        })
        .collect()
}

fn history(sequence: u64, payload: &str) -> Vec<RuntimeHistoryItem> {
    vec![RuntimeHistoryItem {
        sequence,
        kind: RuntimeHistoryKind::HarnessOutput,
        payload: json!(payload),
    }]
}

#[test]
fn start_is_deterministic_and_emits_one_complete_harness_payload() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let first = fixture.runtime.start(fixture.start_input()).unwrap();
    let second = fixture.runtime.start(fixture.start_input()).unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    assert_eq!(first.expected_task_revision, 0);
    assert_eq!(first.expected_goal_revision, 0);
    let [
        KernelCommand {
            payload: KernelCommandPayload::InvokeHarness(invocation),
            ..
        },
    ] = first.commands.as_slice()
    else {
        panic!("one harness invocation")
    };
    assert_eq!(invocation.role, ModelRole::Reasoning);
    assert_eq!(
        invocation.input,
        json!({"repository":"acme/payments","pull_request":142})
    );
    assert_eq!(
        invocation.goal.objective,
        fixture.execution.task.goal.objective
    );
    assert!(invocation.result_schema.validate_definition().is_ok());
    assert_eq!(
        invocation.result_schema.normalized_json()["properties"]["answer"],
        json!({"type":"nullable", "inner":{"type":"string"}})
    );
    assert_eq!(invocation.tools.len(), 0);
    assert!(invocation.projected_history.is_empty());
}

#[test]
fn root_done_returns_the_task_without_completing_the_goal() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let transition = fixture.handle_harness(
        start.snapshot,
        &start.commands[0],
        "ok",
        json!({"answer":"finished traversal"}),
    );
    assert_eq!(
        transition.snapshot.disposition,
        kiteframe::RuntimeDisposition::Returned
    );
    assert!(transition.commands.is_empty());
    assert!(
        !serde_json::to_string(&transition)
            .unwrap()
            .contains("complete_goal")
    );
}

#[test]
fn call_returns_into_the_parent_and_removes_the_joined_child() {
    let fixture = RuntimeFixture::from_source(
        r#"agent call_agent(repository: string, pull_request: integer) -> string {
  entry root(repository = repository, pull_request = pull_request)
  limits { steps: 20 model_turns_per_task: 5 tool_calls_per_response: 2 parallel_branches: 2 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }
  task root(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Delegate.""" tools []
    on delegate(value: string) {
      child_result = call child(value = value) context kept
      done child_result
    }
  }
  task child(value: string) -> string {
    model coding prompt """Finish.""" tools []
    on ok(result: string) { done result }
  }
}"#,
        RuntimeCeilings::default(),
    );
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let after_root = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "delegate",
            json!({"value":"child input"}),
            1,
            "evt-root",
            Uuid::from_u128(21),
            history(0, "root result"),
        )
        .unwrap();
    assert_eq!(after_root.commands.len(), 1);
    let after_child = fixture
        .handle_harness_at(
            after_root.snapshot,
            &after_root.commands[0],
            "ok",
            json!({"result":"child return"}),
            2,
            "evt-child",
            Uuid::from_u128(22),
            history(1, "child result"),
        )
        .unwrap();

    assert_eq!(
        after_child.snapshot.disposition,
        RuntimeDisposition::Returned
    );
    let state = fixture.decode(&after_child.snapshot);
    assert_eq!(state.frames.len(), 1);
    assert_eq!(
        state.frames[&KiteFrameId::new(0)].state,
        KiteFrameState::Returned {
            result: json!("child return")
        }
    );
}

#[test]
fn sequential_child_allocations_can_exceed_255_with_two_live_frames() {
    let mut calls = String::new();
    for index in 0..260 {
        let task = if index % 2 == 0 {
            "even_child"
        } else {
            "odd_child"
        };
        calls.push_str(&format!(
            "      result_{index} = call {task}(value = \"value-{index}\") context isolated\n"
        ));
    }
    let source = format!(
        r#"agent allocation_agent(repository: string, pull_request: integer) -> string {{
  entry root(repository = repository, pull_request = pull_request)
  limits {{ steps: 1000 model_turns_per_task: 10 tool_calls_per_response: 4 parallel_branches: 2 response_item_bytes: 16384 active_execution_seconds: 60 elapsed_seconds: 600 }}
  task root(repository: string, pull_request: integer) -> string {{
    model reasoning prompt """Allocate sequential children.""" tools []
    on run() {{
{calls}      done result_259
    }}
  }}
  task even_child(value: string) -> string {{
    model reasoning prompt """Return even value.""" tools []
    on ok(result: string) {{ done result }}
  }}
  task odd_child(value: string) -> string {{
    model reasoning prompt """Return odd value.""" tools []
    on ok(result: string) {{ done result }}
  }}
}}"#
    );
    let ceilings = RuntimeCeilings {
        max_frames: 2,
        ..RuntimeCeilings::default()
    };
    let fixture = RuntimeFixture::from_source(&source, ceilings);
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let mut transition = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "run",
            json!({}),
            1,
            "evt-root-allocation",
            Uuid::from_u128(1000),
            Vec::new(),
        )
        .unwrap();

    for index in 0..260 {
        let [command] = transition.commands.as_slice() else {
            panic!("allocation {index} should propose one child command")
        };
        transition = fixture
            .handle_harness_at(
                transition.snapshot.clone(),
                command,
                "ok",
                json!({"result": format!("result-{index}")}),
                index + 2,
                &format!("evt-child-allocation-{index}"),
                Uuid::from_u128(1001 + index as u128),
                Vec::new(),
            )
            .unwrap();
    }

    assert_eq!(
        transition.snapshot.disposition,
        RuntimeDisposition::Returned
    );
    let state = fixture.decode(&transition.snapshot);
    assert_eq!(state.frames.len(), 1);
    assert_eq!(state.next_frame_id, 261);
    assert_eq!(
        state.frames[&KiteFrameId::new(0)].state,
        KiteFrameState::Returned {
            result: json!("result-259")
        }
    );
}

#[test]
fn max_frames_rejects_a_parallel_live_frame_overflow() {
    let ceilings = RuntimeCeilings {
        max_frames: 2,
        ..RuntimeCeilings::default()
    };
    let fixture = RuntimeFixture::from_source(
        r#"agent parallel_agent(repository: string, pull_request: integer) -> string {
  entry root(repository = repository, pull_request = pull_request)
  limits { steps: 20 model_turns_per_task: 5 tool_calls_per_response: 2 parallel_branches: 2 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }
  task root(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Split.""" tools []
    on split(left: string, right: string) {
      fork {
        first = call branch(value = left) context isolated
        second = call branch(value = right) context isolated
      } join
      done first
    }
  }
  task branch(value: string) -> string {
    model reasoning prompt """Return.""" tools []
    on ok(result: string) { done result }
  }
}"#,
        ceilings,
    );
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let error = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "split",
            json!({"left":"left", "right":"right"}),
            1,
            "evt-live-frame-overflow",
            Uuid::from_u128(1261),
            Vec::new(),
        )
        .unwrap_err();
    assert_eq!(error, AgentRuntimeError::CeilingExceeded("frames"));
}

#[test]
fn isolated_kept_and_fork_histories_have_exact_boundaries() {
    let fixture = RuntimeFixture::history_graph();
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let after_root = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "split",
            json!({"left":"left-input", "right":"right-input"}),
            1,
            "evt-root",
            Uuid::from_u128(23),
            fixture.root_history(),
        )
        .unwrap();
    let invocations = harness_invocations(&after_root.commands);
    assert_eq!(invocations.len(), 2);
    assert!(invocations[0].projected_history.is_empty());
    assert_eq!(invocations[1].projected_history, fixture.root_history());
    assert_eq!(after_root.commands[0].key.as_str(), "r2-f1-harness");
    assert_eq!(after_root.commands[1].key.as_str(), "r2-f2-harness");
    let left_command = after_root.commands[0].clone();
    let right_command = after_root.commands[1].clone();

    let after_left = fixture
        .handle_harness_at(
            after_root.snapshot,
            &left_command,
            "ok",
            json!({"result":"left return"}),
            2,
            "evt-left",
            Uuid::from_u128(24),
            history(1, "left result"),
        )
        .unwrap();
    // Commands are newly proposed work only. The right sibling's r2 command is
    // still persisted and outstanding, so consuming the left result at r2 may
    // validly produce a Running snapshot with no new command at r3.
    assert!(after_left.commands.is_empty());
    let partial_state = fixture.decode(&after_left.snapshot);
    assert_eq!(
        partial_state.frames[&KiteFrameId::new(0)].state,
        KiteFrameState::WaitingForChildren {
            child_ids: vec![KiteFrameId::new(1), KiteFrameId::new(2)]
        }
    );
    assert_eq!(
        partial_state.frames[&KiteFrameId::new(1)].state,
        KiteFrameState::Returned {
            result: json!("left return")
        }
    );
    assert_eq!(
        partial_state.frames[&KiteFrameId::new(2)].state,
        KiteFrameState::WaitingForHarness {
            command_key: right_command.key.clone()
        }
    );
    let right_state = partial_state.frames[&KiteFrameId::new(2)].clone();
    assert_eq!(right_state.history, fixture.root_history());
    assert!(
        !right_state
            .history
            .iter()
            .any(|item| item.payload == json!("left result"))
    );
    let execution = fixture.execution_with_snapshot(
        after_left.snapshot.clone(),
        2,
        OperationalStatus::WaitingForHarness,
    );
    assert_eq!(
        validate_transition(&after_left, &execution, &RuntimeCeilings::default()),
        Ok(())
    );

    let after_right = fixture
        .handle_harness_at(
            after_left.snapshot,
            &right_command,
            "ok",
            json!({"result":"right return"}),
            3,
            "evt-right",
            Uuid::from_u128(25),
            history(2, "right result"),
        )
        .unwrap();
    assert_eq!(
        after_right.snapshot.disposition,
        RuntimeDisposition::Returned
    );
    assert!(after_right.commands.is_empty());
}

#[test]
fn goto_kept_and_isolated_history_boundaries_are_exact() {
    for (context, expected_history) in [
        ("kept", history(0, "root result")),
        ("isolated", Vec::new()),
    ] {
        let source = format!(
            r#"agent goto_agent(repository: string, pull_request: integer) -> string {{
  entry root(repository = repository, pull_request = pull_request)
  limits {{ steps: 20 model_turns_per_task: 5 tool_calls_per_response: 2 parallel_branches: 2 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task root(repository: string, pull_request: integer) -> string {{
    model reasoning prompt """Root.""" tools []
    on next(value: string) {{ return tail(value = value) context {context} }}
  }}
  task tail(value: string) -> string {{
    model reasoning prompt """Tail.""" tools []
    on ok(result: string) {{ done result }}
  }}
}}"#
        );
        let fixture = RuntimeFixture::from_source(&source, RuntimeCeilings::default());
        let start = fixture.runtime.start(fixture.start_input()).unwrap();
        let next = fixture
            .handle_harness_at(
                start.snapshot,
                &start.commands[0],
                "next",
                json!({"value":"tail input"}),
                1,
                &format!("evt-goto-{context}"),
                Uuid::from_u128(25),
                history(0, "root result"),
            )
            .unwrap();
        let invocations = harness_invocations(&next.commands);
        let [invocation] = invocations.as_slice() else {
            panic!("one tail invocation")
        };
        assert_eq!(invocation.projected_history, expected_history);
        assert_eq!(next.commands[0].key.as_str(), "r2-f0-harness");
    }
}

#[test]
fn isolated_child_gets_its_own_model_turn_ceiling() {
    let fixture = RuntimeFixture::from_source(
        r#"agent turns_agent(repository: string, pull_request: integer) -> string {
  entry root(repository = repository, pull_request = pull_request)
  limits { steps: 20 model_turns_per_task: 5 tool_calls_per_response: 2 parallel_branches: 2 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }
  task root(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Root.""" tools []
    on next(value: string) { result = call child(value = value) context isolated done result }
  }
  task child(value: string) -> string {
    model reasoning prompt """Child.""" tools []
    on ok(result: string) { done result }
  }
}"#,
        RuntimeCeilings::default(),
    );
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let child = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "next",
            json!({"value":"child"}),
            1,
            "evt-turns",
            Uuid::from_u128(26),
            history(0, "root result"),
        )
        .unwrap();
    let invocations = harness_invocations(&child.commands);
    let [invocation] = invocations.as_slice() else {
        panic!("one isolated child invocation")
    };
    assert_eq!(invocation.limits.model_turns_remaining, 5);
}

#[test]
fn current_evidence_can_return_through_a_child_call() {
    let fixture = RuntimeFixture::from_source(
        r#"agent evidence_agent(repository: string, pull_request: integer) -> string {
  entry root(repository = repository, pull_request = pull_request)
  limits { steps: 20 model_turns_per_task: 5 tool_calls_per_response: 2 parallel_branches: 2 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }
  task root(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Root.""" tools []
    on next() { result = call child(evidence = current_evidence) context kept done result }
  }
  task child(evidence: string) -> string {
    model reasoning prompt """Child.""" tools []
    on ok() { done evidence }
  }
}"#,
        RuntimeCeilings::default(),
    );
    let evidence = Uuid::from_u128(27);
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let child = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "next",
            json!({}),
            1,
            "evt-evidence-root",
            evidence,
            history(0, "root result"),
        )
        .unwrap();
    let returned = fixture
        .handle_harness_at(
            child.snapshot,
            &child.commands[0],
            "ok",
            json!({}),
            2,
            "evt-evidence-child",
            Uuid::from_u128(28),
            history(1, "child result"),
        )
        .unwrap();
    assert_eq!(
        fixture.decode(&returned.snapshot).frames[&KiteFrameId::new(0)].state,
        KiteFrameState::Returned {
            result: json!(evidence.to_string())
        }
    );
}

#[test]
fn explicit_complete_and_block_goal_statements_emit_only_their_authority_command() {
    let evidence = Uuid::from_u128(30);
    let complete =
        RuntimeFixture::simple("on completed() { complete goal { finished: current_evidence } }");
    let start = complete.runtime.start(complete.start_input()).unwrap();
    let transition = complete
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "completed",
            json!({}),
            1,
            "evt-complete",
            evidence,
            history(0, "completed"),
        )
        .unwrap();
    assert_eq!(
        transition.snapshot.disposition,
        RuntimeDisposition::Terminating
    );
    let [
        KernelCommand {
            payload: KernelCommandPayload::CompleteGoal(command),
            ..
        },
    ] = transition.commands.as_slice()
    else {
        panic!("one complete-goal command")
    };
    assert_eq!(command.assertions.len(), 1);
    assert_eq!(command.assertions[0].criterion_id, "finished");
    assert_eq!(command.assertions[0].evidence_id.as_uuid(), &evidence);

    let blocked = RuntimeFixture::simple(
        r#"on unavailable(reason: string) {
          block goal when 0 {
            code: "required_access_unavailable"
            explanation: reason
            evidence: [current_evidence]
            unlock: ["Restore access."]
          }
        }"#,
    );
    let start = blocked.runtime.start(blocked.start_input()).unwrap();
    let transition = blocked
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "unavailable",
            json!({"reason":"GitHub is unavailable"}),
            1,
            "evt-block",
            evidence,
            history(0, "blocked"),
        )
        .unwrap();
    assert_eq!(transition.snapshot.disposition, RuntimeDisposition::Blocked);
    let [
        KernelCommand {
            payload: KernelCommandPayload::BlockGoal(command),
            ..
        },
    ] = transition.commands.as_slice()
    else {
        panic!("one block-goal command")
    };
    assert_eq!(command.blocked_when_index, 0);
    assert_eq!(command.evidence_ids[0].as_uuid(), &evidence);
    assert_eq!(command.unlock_conditions, ["Restore access."]);
}

#[test]
fn fail_emits_fail_task_without_goal_authority() {
    let fixture =
        RuntimeFixture::simple(r#"on no_path(reason: string) { fail "no_safe_path" reason }"#);
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let transition = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "no_path",
            json!({"reason":"No safe path remains"}),
            1,
            "evt-fail",
            Uuid::from_u128(31),
            history(0, "failed"),
        )
        .unwrap();
    assert_eq!(
        transition.snapshot.disposition,
        RuntimeDisposition::Terminating
    );
    let [
        KernelCommand {
            payload: KernelCommandPayload::FailTask(command),
            ..
        },
    ] = transition.commands.as_slice()
    else {
        panic!("one fail-task command")
    };
    assert_eq!(command.code, "no_safe_path");
    assert_eq!(command.message, "No safe path remains");
    let wire = serde_json::to_string(&transition.commands).unwrap();
    assert!(!wire.contains("complete_goal"));
    assert!(!wire.contains("block_goal"));
}

#[test]
fn ask_waits_for_a_typed_answer_and_invalid_answer_preserves_the_snapshot() {
    let fixture = RuntimeFixture::simple(
        r#"on clarify(question: string) {
          answer = ask string question
          done answer
        }"#,
    );
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let waiting = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "clarify",
            json!({"question":"Proceed?"}),
            1,
            "evt-ask",
            Uuid::from_u128(32),
            history(0, "asked"),
        )
        .unwrap();
    let [
        KernelCommand {
            key,
            payload: KernelCommandPayload::RequestApproval(command),
            ..
        },
    ] = waiting.commands.as_slice()
    else {
        panic!("one approval request")
    };
    assert_eq!(command.question, "Proceed?");
    assert_eq!(
        command.answer_schema.normalized_json(),
        json!({"type":"string"})
    );

    let supplied = waiting.snapshot.clone();
    let wrong = RuntimeEvent::ApprovalResult {
        event_id: "evt-wrong-answer".into(),
        command_key: key.clone(),
        answer: json!(7),
    };
    assert!(
        fixture
            .handle_event(
                supplied.clone(),
                2,
                OperationalStatus::WaitingForApproval,
                &wrong,
            )
            .is_err()
    );
    assert_eq!(waiting.snapshot, supplied);

    let accepted = RuntimeEvent::ApprovalResult {
        event_id: "evt-answer".into(),
        command_key: key.clone(),
        answer: json!("yes"),
    };
    let returned = fixture
        .handle_event(
            supplied,
            2,
            OperationalStatus::WaitingForApproval,
            &accepted,
        )
        .unwrap();
    assert_eq!(returned.snapshot.disposition, RuntimeDisposition::Returned);
}

#[test]
fn command_failed_for_a_matching_wait_emits_exactly_one_fail_task() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let event = RuntimeEvent::CommandFailed {
        event_id: "evt-command-failed".into(),
        command_key: start.commands[0].key.clone(),
        code: "provider_unavailable".into(),
    };
    let transition = fixture
        .handle_event(
            start.snapshot,
            1,
            OperationalStatus::WaitingForHarness,
            &event,
        )
        .unwrap();
    assert_eq!(
        transition.snapshot.disposition,
        RuntimeDisposition::Terminating
    );
    let [
        KernelCommand {
            payload: KernelCommandPayload::FailTask(command),
            ..
        },
    ] = transition.commands.as_slice()
    else {
        panic!("one fail-task command")
    };
    assert_eq!(command.code, "provider_unavailable");
    assert!(!command.message.is_empty());
    let wire = serde_json::to_string(&transition.commands).unwrap();
    assert!(!wire.contains("complete_goal"));
    assert!(!wire.contains("block_goal"));
}

#[test]
fn hostile_harness_wait_keys_must_match_frame_kind_and_current_revision() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let start = fixture.runtime.start(fixture.start_input()).unwrap();

    for invalid_key in ["r999-f0-harness", "r1-f42-harness", "r1-f0-approval"] {
        let mut payload = start.snapshot.payload.clone();
        payload["frames"]["0"]["state"]["command_key"] = json!(invalid_key);
        let hostile = RuntimeStateSnapshot::new(
            RuntimeDisposition::Running,
            payload,
            &RuntimeCeilings::default(),
        )
        .unwrap();
        let matching_event_command = KernelCommand {
            schema_version: 1,
            key: RuntimeCommandKey::new(invalid_key).unwrap(),
            payload: start.commands[0].payload.clone(),
        };

        assert!(matches!(
            fixture.handle_harness_at(
                hostile,
                &matching_event_command,
                "ok",
                json!({"answer":"forged"}),
                1,
                "evt-hostile-harness-wait",
                Uuid::from_u128(42),
                history(0, "forged"),
            ),
            Err(AgentRuntimeError::InvalidState(_))
        ));
    }
}

#[test]
fn hostile_approval_wait_key_cannot_claim_a_future_revision_or_wrong_frame() {
    let fixture = RuntimeFixture::simple(
        r#"on clarify(question: string) {
          answer = ask string question
          done answer
        }"#,
    );
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let waiting = fixture
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "clarify",
            json!({"question":"Proceed?"}),
            1,
            "evt-hostile-approval-setup",
            Uuid::from_u128(43),
            history(0, "asked"),
        )
        .unwrap();
    let invalid_key = RuntimeCommandKey::new("r999-f42-approval").unwrap();
    let mut payload = waiting.snapshot.payload.clone();
    payload["frames"]["0"]["state"]["command_key"] = json!(invalid_key.as_str());
    let hostile = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        payload,
        &RuntimeCeilings::default(),
    )
    .unwrap();
    let event = RuntimeEvent::ApprovalResult {
        event_id: "evt-hostile-approval".into(),
        command_key: invalid_key,
        answer: json!("yes"),
    };

    assert!(matches!(
        fixture.handle_event(hostile, 2, OperationalStatus::WaitingForApproval, &event,),
        Err(AgentRuntimeError::InvalidState(_))
    ));
}

#[test]
fn non_waiting_consumed_and_malformed_harness_results_are_rejected_without_state_change() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let supplied = start.snapshot.clone();
    let wrong_key = KernelCommand {
        schema_version: 1,
        key: RuntimeCommandKey::new("r1-f99-harness").unwrap(),
        payload: start.commands[0].payload.clone(),
    };
    assert!(
        fixture
            .handle_harness_at(
                supplied.clone(),
                &wrong_key,
                "ok",
                json!({"answer":"x"}),
                1,
                "evt-wrong-key",
                Uuid::from_u128(33),
                history(0, "wrong key"),
            )
            .is_err()
    );
    assert_eq!(start.snapshot, supplied);

    for (outcome, fields) in [
        ("unknown", json!({"answer":"x"})),
        ("ok", json!({})),
        ("ok", json!({"answer":"x", "extra":true})),
        ("ok", json!({"answer":7})),
    ] {
        assert!(
            fixture
                .handle_harness_at(
                    supplied.clone(),
                    &start.commands[0],
                    outcome,
                    fields,
                    1,
                    "evt-invalid-fields",
                    Uuid::from_u128(34),
                    history(0, "invalid"),
                )
                .is_err()
        );
    }

    let forged = json!({
        "type": "harness_result",
        "payload": {
            "event_id": "evt-forged",
            "command_key": "r1-f0-harness",
            "outcome": "ok",
            "fields": {"answer":"x"},
            "evidence_id": Uuid::nil(),
            "history": [],
            "goalPhase": "complete"
        }
    });
    assert!(serde_json::from_value::<RuntimeEvent>(forged).is_err());

    let returned = fixture.handle_harness(
        supplied,
        &start.commands[0],
        "ok",
        json!({"answer":"finished"}),
    );
    assert!(
        fixture
            .handle_harness_at(
                returned.snapshot,
                &start.commands[0],
                "ok",
                json!({"answer":"again"}),
                2,
                "evt-consumed",
                Uuid::from_u128(40),
                history(1, "again"),
            )
            .is_err()
    );
}

#[test]
fn history_definition_and_runtime_ceilings_fail_closed() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let start = fixture.runtime.start(fixture.start_input()).unwrap();
    let duplicate = vec![
        RuntimeHistoryItem {
            sequence: 0,
            kind: RuntimeHistoryKind::HarnessInput,
            payload: json!("input"),
        },
        RuntimeHistoryItem {
            sequence: 0,
            kind: RuntimeHistoryKind::HarnessOutput,
            payload: json!("output"),
        },
    ];
    assert!(
        fixture
            .handle_harness_at(
                start.snapshot.clone(),
                &start.commands[0],
                "ok",
                json!({"answer":"x"}),
                1,
                "evt-duplicate-history",
                Uuid::from_u128(35),
                duplicate,
            )
            .is_err()
    );
    let out_of_order = vec![
        RuntimeHistoryItem {
            sequence: 1,
            kind: RuntimeHistoryKind::HarnessInput,
            payload: json!("input"),
        },
        RuntimeHistoryItem {
            sequence: 0,
            kind: RuntimeHistoryKind::HarnessOutput,
            payload: json!("output"),
        },
    ];
    assert!(
        fixture
            .handle_harness_at(
                start.snapshot.clone(),
                &start.commands[0],
                "ok",
                json!({"answer":"x"}),
                1,
                "evt-out-of-order-history",
                Uuid::from_u128(41),
                out_of_order,
            )
            .is_err()
    );

    let bounded = RuntimeFixture::from_source(
        &fixture.definition.source,
        RuntimeCeilings {
            max_history_items_per_frame: 1,
            ..RuntimeCeilings::default()
        },
    );
    let bounded_start = bounded.runtime.start(bounded.start_input()).unwrap();
    assert!(
        bounded
            .handle_harness_at(
                bounded_start.snapshot,
                &bounded_start.commands[0],
                "ok",
                json!({"answer":"x"}),
                1,
                "evt-history-ceiling",
                Uuid::from_u128(36),
                vec![
                    RuntimeHistoryItem {
                        sequence: 0,
                        kind: RuntimeHistoryKind::HarnessInput,
                        payload: json!("input"),
                    },
                    RuntimeHistoryItem {
                        sequence: 1,
                        kind: RuntimeHistoryKind::HarnessOutput,
                        payload: json!("output"),
                    },
                ],
            )
            .is_err()
    );

    let mut state = fixture.decode(&start.snapshot);
    state.definition_hash = "different-definition".into();
    let mismatched = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        serde_json::to_value(state).unwrap(),
        &RuntimeCeilings::default(),
    )
    .unwrap();
    assert!(
        fixture
            .handle_harness_at(
                mismatched,
                &start.commands[0],
                "ok",
                json!({"answer":"x"}),
                1,
                "evt-hash",
                Uuid::from_u128(37),
                history(0, "hash"),
            )
            .is_err()
    );

    let mut invalid_counters = fixture.decode(&start.snapshot);
    invalid_counters.next_frame_id = 0;
    let invalid_counters = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        serde_json::to_value(invalid_counters).unwrap(),
        &RuntimeCeilings::default(),
    )
    .unwrap();
    assert!(fixture.runtime.decode_snapshot(&invalid_counters).is_err());

    let mut invalid_wait = start.snapshot.payload.clone();
    invalid_wait["frames"]["0"]["state"]["command_key"] = json!("INVALID KEY");
    let invalid_wait = RuntimeStateSnapshot::new(
        RuntimeDisposition::Running,
        invalid_wait,
        &RuntimeCeilings::default(),
    )
    .unwrap();
    assert!(fixture.runtime.decode_snapshot(&invalid_wait).is_err());
}

#[test]
fn statement_command_and_snapshot_ceilings_are_enforced() {
    let steps = RuntimeFixture::from_source(
        r#"agent step_agent(repository: string, pull_request: integer) -> string {
  entry root(repository = repository, pull_request = pull_request)
  limits { steps: 1 model_turns_per_task: 5 tool_calls_per_response: 2 parallel_branches: 2 response_item_bytes: 4096 active_execution_seconds: 30 elapsed_seconds: 60 }
  task root(repository: string, pull_request: integer) -> string {
    model reasoning prompt """Ask.""" tools []
    on clarify(question: string) { answer = ask string question done answer }
  }
}"#,
        RuntimeCeilings::default(),
    );
    let start = steps.runtime.start(steps.start_input()).unwrap();
    let waiting = steps
        .handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "clarify",
            json!({"question":"Proceed?"}),
            1,
            "evt-step-ask",
            Uuid::from_u128(38),
            history(0, "asked"),
        )
        .unwrap();
    let answer = RuntimeEvent::ApprovalResult {
        event_id: "evt-step-answer".into(),
        command_key: waiting.commands[0].key.clone(),
        answer: json!("yes"),
    };
    assert!(matches!(
        steps.handle_event(
            waiting.snapshot,
            2,
            OperationalStatus::WaitingForApproval,
            &answer,
        ),
        Err(AgentRuntimeError::CeilingExceeded("steps"))
    ));

    let parallel = RuntimeFixture::history_graph();
    let limited = RuntimeFixture {
        runtime: KiteAgentRuntime::new(RuntimeCeilings {
            max_commands_per_transition: 1,
            ..RuntimeCeilings::default()
        }),
        execution: parallel.execution,
        definition: parallel.definition,
    };
    let start = limited.runtime.start(limited.start_input()).unwrap();
    assert!(matches!(
        limited.handle_harness_at(
            start.snapshot,
            &start.commands[0],
            "split",
            json!({"left":"left", "right":"right"}),
            1,
            "evt-command-ceiling",
            Uuid::from_u128(39),
            history(0, "split"),
        ),
        Err(AgentRuntimeError::CeilingExceeded(
            "commands_per_transition"
        ))
    ));

    let tiny = RuntimeFixture::from_source(
        &fixture_source(),
        RuntimeCeilings {
            max_snapshot_bytes: 64,
            ..RuntimeCeilings::default()
        },
    );
    assert!(matches!(
        tiny.runtime.start(tiny.start_input()),
        Err(AgentRuntimeError::CeilingExceeded("snapshot_bytes"))
    ));
}

#[test]
fn validation_rejects_non_agent_missing_role_and_legacy_context_contracts() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");
    let mut workflow = fixture.definition.clone();
    workflow.definition_kind = DefinitionKind::Workflow;
    assert!(fixture.runtime.validate_agent(&workflow).is_err());

    let mut missing_role = fixture.definition.clone();
    missing_role.tasks.get_mut("inspect").unwrap().model_role = None;
    assert!(fixture.runtime.validate_agent(&missing_role).is_err());

    let mut legacy = fixture.definition.clone();
    legacy.entry.context = ContextMode::Legacy;
    assert!(fixture.runtime.validate_agent(&legacy).is_err());
}

#[test]
fn validation_rejects_completion_contracts_that_diverge_from_executable_outcomes() {
    let fixture = RuntimeFixture::simple("on ok(answer: string) { done answer }");

    let mut extra_outcome = fixture.definition.clone();
    extra_outcome
        .tasks
        .get_mut("inspect")
        .unwrap()
        .completion
        .outcomes
        .insert("forged".into(), Default::default());

    let mut wrong_fields = fixture.definition.clone();
    wrong_fields
        .tasks
        .get_mut("inspect")
        .unwrap()
        .completion
        .outcomes
        .get_mut("ok")
        .unwrap()
        .insert("answer".into(), CompiledType::Integer);

    let mut wrong_union = fixture.definition.clone();
    wrong_union
        .tasks
        .get_mut("inspect")
        .unwrap()
        .completion
        .required_nullable_fields
        .clear();

    for hostile in [extra_outcome, wrong_fields, wrong_union] {
        assert!(matches!(
            fixture.runtime.validate_agent(&hostile),
            Err(AgentRuntimeError::InvalidDefinition(_))
        ));
    }
}

#[test]
fn validation_requires_each_task_tool_to_match_a_frozen_definition_member() {
    let fixture = RuntimeFixture::with_tool();
    assert_eq!(fixture.definition.tool_bindings.len(), 1);
    assert_eq!(fixture.definition.tasks["inspect"].tools.len(), 1);

    let mut absent_from_definition = fixture.definition.clone();
    absent_from_definition.tool_bindings.clear();

    let replacement = CatalogTool::new(
        "docs/search",
        "docs",
        "A different frozen descriptor.",
        ClosedSchema::object("input", Default::default()),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .unwrap();
    let mut mismatched_member = fixture.definition.clone();
    mismatched_member.tool_bindings[0].descriptor_digest =
        replacement.descriptor_digest().to_owned();
    mismatched_member.tool_bindings[0].descriptor = replacement;

    for hostile in [absent_from_definition, mismatched_member] {
        assert!(matches!(
            fixture.runtime.validate_agent(&hostile),
            Err(AgentRuntimeError::InvalidDefinition(_))
        ));
    }
}

fn fixture_source() -> String {
    RuntimeFixture::simple("on ok(answer: string) { done answer }")
        .definition
        .source
}
