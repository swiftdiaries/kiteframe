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
    let fixture = fs::read_to_string("tests/fixtures/dsl/agent_babysit_pr.kite")
        .expect("agent fixture exists");

    assert!(reference.contains("agent-definition"));
    assert!(reference.contains("workflow-definition (deprecated)"));
    assert!(reference.contains(
        "Agents are immutable and publishable in consolidated Slice 1, but not executable."
    ));
    assert!(reference.contains("`StartRun` rejects agents with `agent_runtime_unavailable`"));
    assert!(!reference.contains("Publication must reject them before persistence"));
    assert!(fixture.starts_with("agent babysit_pr("));
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
