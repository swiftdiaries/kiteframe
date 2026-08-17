use std::fs;

use kiteframe::dsl::{DiagnosticSeverity, ParseLimits, parse_report};

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
fn reviewed_agent_grammar_and_fixture_are_checked_in() {
    let reference =
        fs::read_to_string("docs/reference/kite-language.md").expect("language reference exists");
    let fixture = fs::read_to_string("tests/fixtures/dsl/agent_babysit_pr.kite")
        .expect("agent fixture exists");

    assert!(reference.contains("agent-definition"));
    assert!(reference.contains("workflow-definition (deprecated)"));
    assert!(fixture.starts_with("agent babysit_pr("));
}

#[test]
fn agent_bodies_remain_outside_the_task_two_parser_boundary() {
    let diagnostics = parse_report(BABYSIT, limits())
        .expect_err("nested agent bodies are introduced by the next parser task");

    assert!(diagnostics.iter().all(|diagnostic| {
        diagnostic.severity == DiagnosticSeverity::Error && diagnostic.code != "deprecated_workflow"
    }));
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "expected_top_level_declaration")
    );
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
