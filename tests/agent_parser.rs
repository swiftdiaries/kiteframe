use std::fs;

#[test]
fn reviewed_agent_grammar_and_fixture_are_checked_in() {
    let reference = fs::read_to_string("docs/reference/kite-language.md")
        .expect("language reference exists");
    let fixture = fs::read_to_string("tests/fixtures/dsl/agent_babysit_pr.kite")
        .expect("agent fixture exists");

    assert!(reference.contains("agent-definition"));
    assert!(reference.contains("workflow-definition (deprecated)"));
    assert!(fixture.starts_with("agent babysit_pr("));
}
