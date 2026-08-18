#[test]
fn repository_contains_one_runtime() {
    // Cargo's compiled `CARGO_MANIFEST_DIR` can retain a removed linked-worktree
    // path when test artifacts are reused. Integration tests execute at this
    // package's current working directory, which is the checkout to inspect.
    let root = std::env::current_dir().expect("resolve repository root");
    for removed in [
        "crates",
        "python",
        "fuzz",
        "openfga",
        "runtime-targets",
        "schemas",
        "scripts",
    ] {
        assert!(!root.join(removed).exists(), "obsolete path: {removed}");
    }
    assert!(root.join("src/main.rs").is_file());
}

#[test]
fn published_agent_example_matches_the_reviewed_fixture_byte_for_byte() {
    let fixture = std::fs::read("tests/fixtures/dsl/agent_babysit_pr.kite").unwrap();
    let example = std::fs::read("examples/babysit_pr.kite").unwrap();
    assert_eq!(example, fixture);
}

#[test]
fn foundation_exposes_no_goal_lifecycle_commands() {
    let source = std::fs::read_to_string("src/protocol/types.rs").unwrap();
    for forbidden in ["CompleteGoal", "BlockGoal", "PauseTask", "ResumeTask"] {
        assert!(
            !source.contains(forbidden),
            "unexpected command: {forbidden}"
        );
    }
}
