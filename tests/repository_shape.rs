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
fn public_status_evidence_tracks_completed_slice_three_and_prospective_later_slices() {
    let design = std::fs::read_to_string(
        "docs/superpowers/specs/2026-08-17-kiteframe-declarative-agents-and-plugin-runtime-design.md",
    )
    .unwrap();
    let reference =
        std::fs::read_to_string("docs/reference/agent-runtime-and-goal-lifecycle.md").unwrap();

    let migrations = embedded_migration_versions().unwrap();
    assert_status_evidence(&design, &reference, &migrations).unwrap();

    let stale_head = reference.replacen("..b790a0c", "..d746ce5", 1);
    assert!(
        assert_status_evidence(&design, &stale_head, &migrations).is_err(),
        "status guard accepted stale Slice 3 evidence head"
    );

    let contradictory_design = design.replacen(
        "\n## Normative language",
        "\nSlices 4–8 are implemented and delivered.\n\n## Normative language",
        1,
    );
    assert!(
        assert_status_evidence(&contradictory_design, &reference, &migrations).is_err(),
        "status guard accepted a delivered later-slice claim in the design status"
    );

    let contradictory_reference = reference.replacen(
        "\nCordis auto-composability",
        "\nSlices 4–8 are implemented and delivered.\n\nCordis auto-composability",
        1,
    );
    assert!(
        assert_status_evidence(&design, &contradictory_reference, &migrations).is_err(),
        "status guard accepted a delivered later-slice claim in the evidence"
    );
}

fn assert_status_evidence(
    design: &str,
    reference: &str,
    migration_versions: &[u32],
) -> Result<(), String> {
    let design_header = canonical_section(
        design,
        "# Kiteframe Declarative Agents",
        "## Executive summary",
    )?;
    require_text(
        design_header,
        "Slices 1–3 implemented and Slices 4–8 planned",
        "design header has stale Slice status",
    )?;

    let design_status = canonical_section(
        design,
        "### Non-normative implementation status",
        "## Normative language",
    )?;
    for required in [
        "Slice 3 is implemented: the built-in Responses path, Codex ACP, and DeepSeek",
        "Slices 4–8 remain prospective and unimplemented",
    ] {
        require_text(design_status, required, "design status is incomplete")?;
    }
    if design_status.contains("Slices 3–8 and the end-to-end product loop remain unimplemented") {
        return Err("design status incorrectly records Slice 3 as unimplemented".to_owned());
    }
    assert_no_later_slice_delivery_claim(design_status)?;

    let evidence = canonical_section(
        reference,
        "## Slice 3 implementation evidence — 2026-08-22",
        "\nCordis auto-composability",
    )?;
    for required in [
        "ca09f321672f90da5acca6aa2abf7e69ce6a6694..b790a0c",
        "Initial Tasks 1–7 implementation ended at `d746ce5`",
        "Later production remediations are `af0763b`, `424ffda`, `1a46135`, and `3887425`",
        "CI, documentation, and test evidence fixes are `fa07acd`, `817f65e`, `7eb454c`, and `b790a0c`",
        "Gate B is approved after the `af0763b` and `424ffda` fixes",
        "final APPROVE verdict and zero findings",
        "whole-branch re-review of the final evidence head is still pending",
        "cargo test --test harness_acp_live -- --nocapture",
    ] {
        require_text(evidence, required, "Slice 3 evidence is stale")?;
    }

    let expected_versions = (1..=migration_versions.len() as u32).collect::<Vec<_>>();
    if migration_versions != expected_versions {
        return Err(format!(
            "migration versions are not contiguous from 0001: {migration_versions:?}"
        ));
    }
    let first = migration_versions.first().ok_or("no embedded migrations")?;
    let last = migration_versions.last().ok_or("no embedded migrations")?;
    let migration_evidence = format!(
        "{} migrations (`{first:04}` through `{last:04}`)",
        migration_versions.len()
    );
    require_text(evidence, &migration_evidence, "migration evidence drifted")?;

    let later_slice_scope = reference
        .split_once("\nCordis auto-composability")
        .map(|(_, scope)| scope)
        .ok_or("missing Slice 3 later-slice boundary")?;
    require_text(
        later_slice_scope,
        "Slices 4–8:\nnot implemented by this evidence",
        "later Slice status is missing",
    )?;
    assert_no_later_slice_delivery_claim(evidence)?;
    assert_no_later_slice_delivery_claim(later_slice_scope)?;
    Ok(())
}

fn canonical_section<'a>(
    document: &'a str,
    heading: &str,
    closing_marker: &str,
) -> Result<&'a str, String> {
    let (_, after_heading) = document
        .split_once(heading)
        .ok_or_else(|| format!("missing canonical heading: {heading}"))?;
    let (section, _) = after_heading
        .split_once(closing_marker)
        .ok_or_else(|| format!("missing canonical section boundary: {closing_marker}"))?;
    Ok(section)
}

fn require_text(section: &str, required: &str, message: &str) -> Result<(), String> {
    let normalized_section = section.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized_required = required.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized_section.contains(&normalized_required) {
        Ok(())
    } else {
        Err(format!("{message}: {required}"))
    }
}

fn assert_no_later_slice_delivery_claim(section: &str) -> Result<(), String> {
    let normalized = section.to_ascii_lowercase().replace('–', "-");
    for statement in normalized.split(['.', '\n']) {
        let mentions_later_slice = statement.contains("slices 4-8")
            || (4..=8).any(|slice| statement.contains(&format!("slice {slice}")));
        let claims_delivery = ["implemented", "delivered", "operational"]
            .iter()
            .any(|claim| statement.contains(claim));
        let preserves_prospective_status = [
            "not implemented",
            "unimplemented",
            "prospective",
            "deferred",
            "planned",
        ]
        .iter()
        .any(|status| statement.contains(status));
        if mentions_later_slice && claims_delivery && !preserves_prospective_status {
            return Err(format!(
                "contradictory later-slice delivery claim: {statement}"
            ));
        }
    }
    Ok(())
}

fn embedded_migration_versions() -> Result<Vec<u32>, String> {
    let mut versions = std::fs::read_dir("migrations")
        .map_err(|error| format!("read migrations: {error}"))?
        .map(|entry| entry.map_err(|error| format!("read migration entry: {error}")))
        .filter_map(|entry| match entry {
            Ok(entry)
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "sql") =>
            {
                Some(Ok(entry))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .map(|entry| {
            let entry = entry?;
            entry
                .file_name()
                .to_string_lossy()
                .split_once('_')
                .map(|(prefix, _)| prefix.parse::<u32>())
                .ok_or_else(|| {
                    format!("migration lacks numeric prefix: {}", entry.path().display())
                })?
                .map_err(|error| format!("invalid migration prefix: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    versions.sort_unstable();
    Ok(versions)
}

#[test]
fn public_testing_guide_locks_complete_live_acp_certification_examples() {
    let guide = std::fs::read_to_string("docs/guides/testing-kiteframe.md").unwrap();
    assert_live_acp_certification_examples(&guide).unwrap();

    for (field, expected, replacement) in [
        (
            "Codex command",
            r#"["/absolute/codex-acp"]"#,
            r#"["/wrong/codex-acp"]"#,
        ),
        (
            "Codex revision",
            "296069e841634cd4bb9bc4515602d836e49231ec",
            "0000000000000000000000000000000000000001",
        ),
        (
            "Codex authentication map",
            r#"{"OPENAI_API_KEY":"<obtain-from-secret-manager-or-session>"}"#,
            r#"{"OPENAI_API_KEY":"<incorrect-placeholder>"}"#,
        ),
        (
            "Codex required key list",
            r#"["OPENAI_API_KEY"]"#,
            r#"["WRONG_KEY"]"#,
        ),
        (
            "DeepSeek command",
            r#"["pnpm","--dir","/absolute/deepseek-harness","run","demo:acp"]"#,
            r#"["pnpm","run","wrong:acp"]"#,
        ),
        (
            "DeepSeek revision",
            "141eb6fef83422698aef7a981029e843e8161534",
            "0000000000000000000000000000000000000002",
        ),
        (
            "DeepSeek authentication map",
            r#"{"DEEPSEEK_API_KEY":"<obtain-from-secret-manager-or-session>","PATH":"/absolute/bin"}"#,
            r#"{"DEEPSEEK_API_KEY":"<incorrect-placeholder>","PATH":"/absolute/bin"}"#,
        ),
        (
            "DeepSeek required key list",
            r#"["DEEPSEEK_API_KEY"]"#,
            r#"["WRONG_KEY"]"#,
        ),
    ] {
        let mutated = guide.replacen(expected, replacement, 1);
        assert!(
            assert_live_acp_certification_examples(&mutated).is_err(),
            "guard accepted drift in {field}"
        );
    }
}

fn assert_live_acp_certification_examples(guide: &str) -> Result<(), String> {
    for (name, snippet) in [
        (
            "Codex authentication map",
            r#"export KITEFRAME_CODEX_ACP_ENV_JSON='{"OPENAI_API_KEY":"<obtain-from-secret-manager-or-session>"}'"#,
        ),
        (
            "Codex required key list",
            r#"export KITEFRAME_CODEX_ACP_REQUIRED_ENV_JSON='["OPENAI_API_KEY"]'"#,
        ),
        (
            "Codex command and revision",
            r#"KITEFRAME_CODEX_ACP_COMMAND_JSON='["/absolute/codex-acp"]' KITEFRAME_CODEX_ACP_SOURCE_REVISION='296069e841634cd4bb9bc4515602d836e49231ec' cargo test --test harness_acp_live codex_acp_live_core -- --exact --nocapture"#,
        ),
        (
            "DeepSeek authentication map and PATH",
            r#"export KITEFRAME_DSH_ACP_ENV_JSON='{"DEEPSEEK_API_KEY":"<obtain-from-secret-manager-or-session>","PATH":"/absolute/bin"}'"#,
        ),
        (
            "DeepSeek required key list",
            r#"export KITEFRAME_DSH_ACP_REQUIRED_ENV_JSON='["DEEPSEEK_API_KEY"]'"#,
        ),
        (
            "DeepSeek command and revision",
            r#"KITEFRAME_DSH_ACP_COMMAND_JSON='["pnpm","--dir","/absolute/deepseek-harness","run","demo:acp"]' KITEFRAME_DSH_ACP_SOURCE_REVISION='141eb6fef83422698aef7a981029e843e8161534' cargo test --test harness_acp_live deepseek_harness_acp_live_core -- --exact --nocapture"#,
        ),
    ] {
        if !guide.contains(snippet) {
            return Err(format!("missing or drifted {name}"));
        }
    }
    Ok(())
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

    assert_ci_isolation(&parse_ci_workflow(&workflow).unwrap()).unwrap();
}

#[test]
fn ci_structural_guard_rejects_decoy_comment_and_non_rust_job() {
    let workflow = std::fs::read_to_string(".github/workflows/ci.yml").unwrap();
    let missing_create = workflow.replacen(
        "- name: Create isolated Python live-smoke database",
        "- name: Decoy create step",
        1,
    );
    let decoy_comment =
        format!("# - name: Create isolated Python live-smoke database\n{missing_create}");
    assert!(assert_ci_isolation(&parse_ci_workflow(&decoy_comment).unwrap()).is_err());

    let non_rust_job = workflow.replacen("  rust:\n", "  decoy:\n", 1);
    assert!(assert_ci_isolation(&parse_ci_workflow(&non_rust_job).unwrap()).is_err());
}

#[derive(Debug, Deserialize)]
struct CiWorkflow {
    jobs: BTreeMap<String, CiJob>,
}

#[derive(Debug, Deserialize)]
struct CiJob {
    steps: Vec<CiStep>,
}

#[derive(Debug, Deserialize)]
struct CiStep {
    id: Option<String>,
    name: Option<String>,
    run: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(rename = "if")]
    condition: Option<String>,
    #[serde(rename = "working-directory")]
    working_directory: Option<String>,
}

fn parse_ci_workflow(workflow: &str) -> Result<CiWorkflow, yaml_serde::Error> {
    yaml_serde::from_str(workflow)
}

fn assert_ci_isolation(workflow: &CiWorkflow) -> Result<(), String> {
    let rust_steps = &workflow.jobs.get("rust").ok_or("missing rust job")?.steps;
    let rust_tests = required_step(rust_steps, "Test all targets")?;
    let create = required_step(rust_steps, "Create isolated Python live-smoke database")?;
    let smoke = required_step(
        rust_steps,
        "Exercise Python client against the compiled harness",
    )?;
    let cleanup = required_step(rust_steps, "Drop isolated Python live-smoke database")?;
    let position = |step: &CiStep| {
        rust_steps
            .iter()
            .position(|candidate| std::ptr::eq(candidate, step))
            .expect("step originates from rust job")
    };
    if !(position(rust_tests) < position(create)
        && position(create) < position(smoke)
        && position(smoke) < position(cleanup))
    {
        return Err("Rust CI steps are out of order".to_owned());
    }

    let database_name = "kiteframe_ci_python_live_${{ github.run_id }}_${{ github.run_attempt }}";
    assert_step(
        create,
        Some("python-live-smoke-database"),
        Some(
            "python3 tests/ci/isolated_database.py create \\\n  --database-url \"$DATABASE_URL\" \\\n  --database-name \"$KITEFRAME_SMOKE_DATABASE_NAME\" \\\n  --github-output \"$GITHUB_OUTPUT\"",
        ),
        Some(database_name),
        None,
    )?;
    if smoke
        .env
        .get("KITEFRAME_INTEGRATION_DATABASE_URL")
        .map(String::as_str)
        != Some("${{ steps.python-live-smoke-database.outputs.database_url }}")
    {
        return Err("Python smoke does not consume the isolated database output".to_owned());
    }
    assert_step(
        cleanup,
        None,
        Some(
            "python3 tests/ci/isolated_database.py drop \\\n  --database-url \"$DATABASE_URL\" \\\n  --database-name \"$KITEFRAME_SMOKE_DATABASE_NAME\"",
        ),
        Some(database_name),
        Some("${{ always() }}"),
    )?;

    let python_steps = &workflow
        .jobs
        .get("python")
        .ok_or("missing python job")?
        .steps;
    let pyright = required_step(python_steps, "Type-check Python client")?;
    if pyright.working_directory.as_deref() != Some("clients/python")
        || pyright.run.as_deref() != Some("uv run pyright")
    {
        return Err("Pyright does not run from the Python client directory".to_owned());
    }
    Ok(())
}

fn required_step<'a>(steps: &'a [CiStep], name: &str) -> Result<&'a CiStep, String> {
    let mut matching = steps
        .iter()
        .filter(|step| step.name.as_deref() == Some(name));
    let step = matching
        .next()
        .ok_or_else(|| format!("missing step: {name}"))?;
    if matching.next().is_some() {
        return Err(format!("duplicate step: {name}"));
    }
    Ok(step)
}

fn assert_step(
    step: &CiStep,
    id: Option<&str>,
    run: Option<&str>,
    database_name: Option<&str>,
    condition: Option<&str>,
) -> Result<(), String> {
    if step.id.as_deref() != id
        || step.run.as_deref().map(str::trim) != run
        || step
            .env
            .get("KITEFRAME_SMOKE_DATABASE_NAME")
            .map(String::as_str)
            != database_name
        || step.condition.as_deref() != condition
    {
        return Err(format!(
            "incorrect CI step: {}; id={:?}; run={:?}; env={:?}; condition={:?}",
            step.name.as_deref().unwrap_or("unnamed"),
            step.id,
            step.run,
            step.env,
            step.condition
        ));
    }
    Ok(())
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
use serde::Deserialize;
use std::collections::BTreeMap;
