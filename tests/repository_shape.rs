#[test]
fn repository_contains_one_runtime() {
    // Cargo's compiled `CARGO_MANIFEST_DIR` can retain a removed linked-worktree
    // path when test artifacts are reused. Integration tests execute at this
    // package's current working directory, which is the checkout to inspect.
    let root = std::env::current_dir().expect("resolve repository root");
    for removed in [
        "crates",
        "python",
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

#[test]
fn public_docs_name_both_v1_acp_profiles_and_exclude_dynamic_cordis() {
    let readme = std::fs::read_to_string("README.md").unwrap();
    let guide = std::fs::read_to_string("docs/guides/building-on-kiteframe.md").unwrap();
    assert!(readme.contains("Codex ACP and DeepSeek Harness ACP"));
    assert!(guide.contains("portable ACP core"));
    assert!(guide.contains("Cordis auto-composability is deferred from V1"));
}

#[test]
fn no_v1_runtime_surface_exposes_cordis_package_lifecycle() {
    let mut sources = Vec::new();
    collect_rust_sources(std::path::Path::new("src"), &mut sources);

    for source in sources {
        let text = std::fs::read_to_string(&source).unwrap();
        for forbidden in [
            "cordis_define",
            "cordis_run",
            "cordis_stop",
            "CompositionProposed",
            "PackageActivated",
        ] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} leaked into {}",
                source.display()
            );
        }
    }
}

#[test]
fn ci_runs_the_python_live_smoke_in_a_disposable_database_after_rust_tests() {
    let workflow = std::fs::read_to_string(".github/workflows/ci.yml").unwrap();

    let rust_tests = workflow.find("- name: Test all targets").unwrap();
    let create = workflow
        .find("- name: Create isolated Python live-smoke database")
        .unwrap();
    let smoke = workflow
        .find("- name: Exercise Python client against the compiled harness")
        .unwrap();
    let drop = workflow
        .find("- name: Drop isolated Python live-smoke database")
        .unwrap();

    assert!(rust_tests < create && create < smoke && smoke < drop);
    for required in [
        "tests/ci/isolated_database.py create",
        "tests/ci/isolated_database.py drop",
        "if: ${{ always() }}",
        "KITEFRAME_INTEGRATION_DATABASE_URL: ${{ steps.python-live-smoke-database.outputs.database_url }}",
        "working-directory: clients/python\n        run: uv run pyright",
    ] {
        assert!(
            workflow.contains(required),
            "CI workflow is missing: {required}"
        );
    }
}

fn collect_rust_sources(directory: &std::path::Path, sources: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}
