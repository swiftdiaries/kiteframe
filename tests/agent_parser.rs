use std::fs;

use kiteframe::dsl::{
    ContextModeAst, DefinitionAst, ModelRoleAst, ParseLimits, StatementAst, parse_report,
};

const BABYSIT: &str = include_str!("fixtures/dsl/agent_babysit_pr.kite");

fn limits() -> ParseLimits {
    ParseLimits {
        max_source_bytes: 32 * 1024,
        max_tokens: 4_096,
        max_identifier_bytes: 128,
        max_nesting: 32,
        max_diagnostics: 8,
    }
}

#[test]
fn reviewed_agent_language_contract_is_checked_in() {
    let reference =
        fs::read_to_string("docs/reference/kite-language.md").expect("language reference exists");
    let runtime = fs::read_to_string("docs/reference/agent-runtime-and-goal-lifecycle.md")
        .expect("runtime reference exists");
    let fixture = fs::read_to_string("tests/fixtures/dsl/agent_babysit_pr.kite")
        .expect("agent fixture exists");

    assert!(reference.contains("agent-definition"));
    assert!(reference.contains("workflow-definition (deprecated)"));
    assert!(reference.contains("complete-goal"));
    assert!(reference.contains("block-goal"));
    assert!(reference.contains("`reserved_current_evidence_binding`"));
    assert!(reference.contains("Legacy `StartRun` still rejects agents"));
    assert!(!reference.contains("Publication must reject them before persistence"));
    assert!(runtime.contains("snapshot bytes | 262,144"));
    assert!(runtime.contains("`HarnessResult` supplies only an invocation outcome"));
    assert!(fixture.starts_with("agent babysit_pr("));
    assert!(fixture.contains("complete goal"));
    assert!(fixture.contains("block goal when 0"));
    assert!(fixture.ends_with("}\n"));
}

#[test]
fn parses_nested_agent_tasks_roles_events_and_elapsed_limit() {
    let report = parse_report(BABYSIT, limits()).expect("reviewed agent parses");
    assert!(report.diagnostics.is_empty());
    let DefinitionAst::Agent(agent) = report.source.definition else {
        panic!("fixture must be an agent");
    };
    assert_eq!(agent.name, "babysit_pr");
    assert_eq!(agent.entry.task, "inspect_pr");
    assert_eq!(agent.events[0].event_type, "github.pull_request.changed.v1");
    assert_eq!(agent.limits["elapsed_seconds"], 604_800);
    assert_eq!(agent.tasks.len(), 3);
    assert_eq!(agent.tasks[0].model_role, Some(ModelRoleAst::Reasoning));
}

#[test]
fn parses_agent_traversal_shapes_and_context_modes() {
    let report = parse_report(BABYSIT, limits()).expect("reviewed agent parses");
    let DefinitionAst::Agent(agent) = report.source.definition else {
        panic!("fixture must be an agent");
    };
    assert_eq!(agent.entry.context, ContextModeAst::Isolated);
    let inspect = &agent.tasks[0];
    assert!(matches!(
        inspect.outcomes[0].statements.as_slice(),
        [StatementAst::Call(call), StatementAst::Done(_)] if call.context == ContextModeAst::Kept
    ));
    assert!(matches!(
        inspect.outcomes[1].statements.as_slice(),
        [StatementAst::Goto(call)] if call.context == ContextModeAst::Isolated
    ));
    assert!(matches!(
        inspect.outcomes[2].statements.as_slice(),
        [StatementAst::Parallel(calls), StatementAst::Done(_)]
            if calls.len() == 2 && calls.iter().all(|call| call.context == ContextModeAst::Isolated)
    ));
}

#[test]
fn rejects_agent_declaration_cardinality_and_definition_separation_violations() {
    let agent_task =
        r#"task t() -> string { model reasoning prompt """x""" tools [] on ok() { done "x" } }"#;
    for (source, expected_code) in [
        (
            format!("agent a() -> string {{ entry t() entry t() {agent_task} }}"),
            "duplicate_agent_entry",
        ),
        (
            format!("agent a() -> string {{ {agent_task} }}"),
            "missing_agent_entry",
        ),
        (
            format!("agent a() -> string {{ entry t() events [\"github.pull_request.changed.v1\"] events [\"github.review.submitted.v1\"] {agent_task} }}"),
            "duplicate_agent_events",
        ),
        (
            format!("agent a() -> string {{ entry t() events [\"github.pull_request.changed.v1\", \"github.pull_request.changed.v1\"] {agent_task} }}"),
            "duplicate_agent_event",
        ),
        (
            format!("agent a() -> string {{ entry t() events [\"github.pull_request.changed\"] {agent_task} }}"),
            "invalid_agent_event",
        ),
        (
            format!("agent a() -> string {{ entry t() events [] {agent_task} }}"),
            "empty_agent_events",
        ),
        ("agent a() -> string { entry t() }".to_owned(), "missing_agent_task"),
        (
            format!("agent a() -> string {{ entry t() agent nested() -> string {{ entry t() {agent_task} }} {agent_task} }}"),
            "nested_agent_not_allowed",
        ),
        (
            format!("agent a() -> string {{ entry t() {agent_task} }} task outside() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"x\" }} }}"),
            "unexpected_top_level_task",
        ),
        (
            "agent a() -> string { entry t() task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok() { done \"x\" } } }".to_owned(),
            "missing_model_role",
        ),
        (
            "agent a() -> string { entry t() task t() -> string { model reasoning model coding prompt \"\"\"x\"\"\" tools [] on ok() { done \"x\" } } }".to_owned(),
            "duplicate_agent_task_model",
        ),
        (
            "task t() -> string { model reasoning prompt \"\"\"x\"\"\" tools [] on ok() { done \"x\" } }".to_owned(),
            "model_role_requires_agent",
        ),
    ] {
        let diagnostics = parse_report(&source, limits()).expect_err("invalid declaration rejected");
        assert_eq!(diagnostics[0].code, expected_code, "{source}");
        assert!(diagnostics.len() <= limits().max_diagnostics, "{source}");
    }
}

#[test]
fn rejects_agent_only_elapsed_limit_on_workflows_with_an_exact_code() {
    let source = "workflow w() -> string { start t() limits { elapsed_seconds: 1 } } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok() { done \"x\" } }";
    let diagnostics = parse_report(source, limits()).expect_err("agent-only limit rejected");

    assert_eq!(diagnostics[0].code, "deprecated_workflow");
    assert_eq!(diagnostics[1].code, "elapsed_seconds_requires_agent");
    assert!(diagnostics.len() <= limits().max_diagnostics);
}

#[test]
fn rejects_invalid_agent_traversal_syntax_and_legacy_separation_violations() {
    let task = |body: &str| {
        format!(
            "agent a() -> string {{ entry t() task t() -> string {{ model reasoning prompt \"\"\"x\"\"\" tools [] on ok() {{ {body} }} }} }}"
        )
    };
    for (source, expected_code) in [
        (
            task("fork { branch = call t() } done \"x\""),
            "missing_fork_join",
        ),
        (task("fork { } join done \"x\""), "empty_fork"),
        (
            task("fork { call t() } join done \"x\""),
            "fork_call_requires_binding",
        ),
        (
            task("answer = call t() context summarized done answer"),
            "invalid_context_mode",
        ),
        (task("goto t()"), "goto_requires_workflow"),
        (
            "task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok() { return t() } }"
                .to_owned(),
            "return_requires_agent",
        ),
    ] {
        let diagnostics = parse_report(&source, limits()).expect_err("invalid source rejected");
        assert_eq!(diagnostics[0].code, expected_code, "{source}");
        assert!(diagnostics.len() <= limits().max_diagnostics, "{source}");
    }
}

#[test]
fn workflow_tag_does_not_accept_agent_only_events() {
    let diagnostics = parse_report(
        r#"workflow w() -> string { events ["github.pull_request.changed.v1"] start t() }
task t() -> string { prompt """x""" tools [] on ok() { done "x" } }"#,
        limits(),
    )
    .expect_err("events remain agent-only syntax");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "invalid_workflow_body")
    );
}

#[test]
fn parses_explicit_agent_goal_authority_statements() {
    let source = r#"
agent goal_agent(blocker: string) -> string {
  entry work(blocker = blocker)
  task work(blocker: string) -> string {
    model reasoning
    prompt """Choose a declared terminal path."""
    tools []
    on complete_it() {
      complete goal { criterion_one: current_evidence }
    }
    on block_it() {
      block goal when 0 {
        code: "required_actor_unavailable"
        explanation: blocker
        evidence: [current_evidence]
        unlock: ["The required actor responds."]
      }
    }
  }
}
"#;
    let report = parse_report(source, limits()).unwrap();
    let DefinitionAst::Agent(agent) = report.source.definition else {
        panic!()
    };
    assert!(matches!(
        agent.tasks[0].outcomes[0].statements[0],
        StatementAst::CompleteGoal(_),
    ));
    assert!(matches!(
        agent.tasks[0].outcomes[1].statements[0],
        StatementAst::BlockGoal(_),
    ));
}

fn goal_authority_agent() -> String {
    r#"
agent goal_agent(blocker: string) -> string {
  entry work(blocker = blocker)
  task work(blocker: string) -> string {
    model reasoning
    prompt """Choose a declared terminal path."""
    tools []
    on complete_it() {
      complete goal { criterion_one: current_evidence }
    }
    on block_it() {
      block goal when 0 {
        code: "required_actor_unavailable"
        explanation: blocker
        evidence: [current_evidence]
        unlock: ["The required actor responds."]
      }
    }
  }
}
"#
    .to_owned()
}

#[test]
fn goal_authority_syntax_fails_closed() {
    let agent = goal_authority_agent();
    let cases = [
        (
            agent.replace("criterion_one: current_evidence", ""),
            "empty_goal_completion",
        ),
        (
            agent.replace(
                "criterion_one: current_evidence",
                "criterion_one: current_evidence criterion_one: current_evidence",
            ),
            "duplicate_goal_criterion",
        ),
        (
            agent.replace("criterion_one", "Bad-ID"),
            "invalid_goal_criterion",
        ),
        (
            agent.replace("when 0", "when 65536"),
            "invalid_blocked_when_index",
        ),
        (
            agent.replace("when 0", "when later"),
            "invalid_blocked_when_index",
        ),
        (
            agent.replace("evidence: [current_evidence]", "evidence: []"),
            "empty_block_evidence",
        ),
        (
            agent.replace("unlock: [\"The required actor responds.\"]", "unlock: []"),
            "empty_unlock_conditions",
        ),
        (
            agent.replace("explanation: blocker", "explanation: 1"),
            "goal_authority_value_must_be_string",
        ),
        (
            agent.replace("code: \"required_actor_unavailable\"", "code: 1"),
            "goal_authority_value_must_be_string",
        ),
        (
            agent.replace("evidence: [current_evidence]", "evidence: [1]"),
            "goal_authority_value_must_be_string",
        ),
        (
            agent.replace("unlock: [\"The required actor responds.\"]", "unlock: [1]"),
            "goal_authority_value_must_be_string",
        ),
        (
            agent.replace(
                "code: \"required_actor_unavailable\"",
                "unknown: \"required_actor_unavailable\"",
            ),
            "missing_block_goal_field",
        ),
        (
            agent.replace(
                "code: \"required_actor_unavailable\"",
                "code: \"required_actor_unavailable\" code: \"again\"",
            ),
            "duplicate_block_goal_field",
        ),
        (
            agent.replace("explanation: blocker", ""),
            "missing_block_goal_field",
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(
            parse_report(&source, limits()).unwrap_err()[0].code,
            expected,
            "{source}",
        );
    }
}

#[test]
fn hyphenated_identifier_like_text_remains_invalid_outside_goal_criteria() {
    let diagnostics = parse_report(
        "agent bad-name() -> string { entry work() task work() -> string { model reasoning prompt \"\"\"x\"\"\" tools [] on ok() { done \"x\" } } }",
        limits(),
    )
    .unwrap_err();

    assert_eq!(diagnostics[0].code, "expected_identifier");
}

#[test]
fn goal_authority_operations_are_agent_only_and_terminal() {
    let complete_workflow = r#"
workflow w() -> string { start t() }
task t() -> string {
  prompt """x"""
  tools []
  on ok() { complete goal { criterion_one: "evidence" } }
}
"#;
    let block_workflow = complete_workflow.replace(
        "complete goal { criterion_one: \"evidence\" }",
        "block goal when 0 { code: \"blocked\" explanation: \"blocked\" evidence: [\"evidence\"] unlock: [\"later\"] }",
    );
    for (source, expected) in [
        (complete_workflow.to_owned(), "complete_goal_requires_agent"),
        (block_workflow, "block_goal_requires_agent"),
    ] {
        let diagnostics = parse_report(&source, limits()).unwrap_err();
        assert!(diagnostics.iter().any(|item| item.code == expected));
    }

    for terminal in [
        "complete goal { criterion_one: current_evidence }",
        "block goal when 0 { code: \"blocked\" explanation: blocker evidence: [current_evidence] unlock: [\"later\"] }",
    ] {
        let source = goal_authority_agent().replace(
            "complete goal { criterion_one: current_evidence }",
            &format!("{terminal} done blocker"),
        );
        assert_eq!(
            parse_report(&source, limits()).unwrap_err()[0].code,
            "statement_after_terminal",
        );
    }
}
