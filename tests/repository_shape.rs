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
fn slice_two_does_not_pull_later_slices_into_the_repository() {
    let root = std::env::current_dir().unwrap();
    for absent in [
        "src/plugin",
        "src/connector",
        "src/channel",
        "src/work_item",
        "src/budget",
        "src/profile",
        "clients/editor",
        "python/connectors",
    ] {
        assert!(
            !root.join(absent).exists(),
            "later-slice path exists: {absent}"
        );
    }

    let source = std::fs::read_to_string("src/agent/types.rs").unwrap();
    for forbidden in [
        "provider_id",
        "model_name",
        "credential",
        "endpoint",
        "protocol_version",
        "capability_negotiation",
        "outcome_unknown",
    ] {
        assert!(
            !source.contains(forbidden),
            "later-slice field: {forbidden}"
        );
    }

    for path in [
        "examples/babysit_pr.kite",
        "tests/fixtures/dsl/agent_babysit_pr.kite",
    ] {
        let kite = std::fs::read_to_string(path).unwrap();
        for forbidden in [
            "provider_id",
            "model_name",
            "credential",
            "endpoint",
            "subscription_id",
            "runtime_id",
        ] {
            assert!(
                !kite.contains(forbidden),
                "runtime choice in {path}: {forbidden}"
            );
        }
    }
}

#[test]
fn private_control_plane_exposes_task_lifecycle_but_not_goal_authority() {
    let source = std::fs::read_to_string("src/protocol/types.rs").unwrap();
    for required in [
        "StartAgent",
        "GetAgentExecution",
        "PauseTask",
        "ResumeTask",
        "CancelTask",
        "ReplayAgentEvents",
    ] {
        assert!(source.contains(required), "missing command: {required}");
    }
    for forbidden in ["CompleteGoal", "BlockGoal"] {
        assert!(
            !source.contains(forbidden),
            "unexpected command: {forbidden}"
        );
    }
}
