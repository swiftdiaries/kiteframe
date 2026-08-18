use std::collections::BTreeSet;

use kiteframe::dsl::{
    CompiledDefinition, CompiledStatement, ContextMode, DefinitionKind, Limits, ModelRole,
    compile_report,
};
use kiteframe::{
    CatalogTool, ClosedSchema, EffectKind, RecoverySupport, SessionId, ToolCatalogSnapshot,
    canonical_sha256,
};
use serde_json::json;
use uuid::Uuid;

const BABYSIT: &str = include_str!("fixtures/dsl/agent_babysit_pr.kite");

fn ceilings() -> Limits {
    Limits {
        steps: 200,
        model_turns_per_task: 12,
        tool_calls_per_response: 8,
        parallel_branches: 4,
        response_item_bytes: 16 * 1024,
        active_execution_seconds: 300,
        elapsed_seconds: 604_800,
    }
}

fn tool(qualified_name: &str) -> CatalogTool {
    let server_alias = qualified_name
        .split_once('/')
        .expect("test tools have qualified names")
        .0;
    CatalogTool::new(
        qualified_name,
        server_alias,
        "A visible test tool.",
        ClosedSchema::object("input", Default::default()),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("test descriptor is valid")
}

fn catalog(tools: impl IntoIterator<Item = CatalogTool>) -> ToolCatalogSnapshot {
    ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(7), tools)
        .expect("test catalog is valid")
}

#[test]
fn version_one_json_defaults_to_workflow_and_legacy_context() {
    let json = include_str!("fixtures/dsl/legacy_minimal.compiled.json");
    let compiled: CompiledDefinition = serde_json::from_str(json).unwrap();
    assert_eq!(compiled.definition_kind, DefinitionKind::Workflow);
    assert_eq!(compiled.entry.context, ContextMode::Legacy);
    assert!(compiled.events.is_empty());
    assert!(
        compiled
            .tasks
            .values()
            .all(|task| task.model_role.is_none())
    );

    let round_trip_json =
        serde_json::to_string_pretty(&compiled).expect("legacy definition serializes") + "\n";
    assert_eq!(round_trip_json, json);

    let round_trip = serde_json::to_value(&compiled).unwrap();
    assert!(round_trip.get("definition_kind").is_none());
    assert!(round_trip.get("events").is_none());
}

#[test]
fn compiles_agent_metadata_and_reuses_graph_safety_checks() {
    let result = compile_report(BABYSIT, &catalog([tool("github/context")]), &ceilings())
        .expect("reviewed agent compiles");
    assert!(result.diagnostics.is_empty());
    let compiled = result.definition;
    assert_eq!(compiled.dsl_version, 2);
    assert_eq!(compiled.definition_kind, DefinitionKind::Agent);
    assert_eq!(compiled.events.len(), 3);
    assert_eq!(
        compiled.tasks["repair_reviews"].model_role,
        Some(ModelRole::Coding),
    );
    let CompiledStatement::Call { call, .. } =
        &compiled.tasks["inspect_pr"].outcomes["repair"].statements[0]
    else {
        panic!("repair starts with a compiled call");
    };
    assert_eq!(call.context, ContextMode::Kept);
    assert_eq!(compiled.tool_bindings.len(), 1);
    assert_eq!(compiled.tool_bindings[0].qualified_name, "github/context");

    let CompiledStatement::Goto { call: refresh } =
        &compiled.tasks["inspect_pr"].outcomes["refresh"].statements[0]
    else {
        panic!("refresh starts with a compiled return");
    };
    let CompiledStatement::Parallel { calls: compare } =
        &compiled.tasks["inspect_pr"].outcomes["compare"].statements[0]
    else {
        panic!("compare starts with a compiled fork");
    };
    let actual = json!({
        "definition_hash": compiled.definition_hash,
        "definition_kind": compiled.definition_kind,
        "dsl_version": compiled.dsl_version,
        "events": compiled.events,
        "model_roles": compiled.tasks.iter().map(|(name, task)| {
            (name.clone(), task.model_role)
        }).collect::<std::collections::BTreeMap<_, _>>(),
        "contexts": {
            "entry": compiled.entry.context,
            "repair": call.context,
            "refresh": refresh.context,
            "compare": compare.iter().map(|binding| binding.call.context).collect::<Vec<_>>(),
        },
        "tool_bindings": compiled.tool_bindings,
    });
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/dsl/agent_babysit_pr.compiled.json"))
            .expect("agent golden is valid JSON");
    assert_eq!(actual, expected);
}

#[test]
fn agent_identity_binds_normalized_kind_events_roles_and_contexts() {
    let descriptor = tool("github/context");
    let compiled = compile_report(BABYSIT, &catalog([descriptor.clone()]), &ceilings())
        .expect("reviewed agent compiles")
        .definition;
    let identity = json!({
        "dsl_version": 2,
        "definition_kind": "agent",
        "source": BABYSIT,
        "events": [
            "github.pull_request.changed.v1",
            "github.review.submitted.v1",
            "github.required_checks.changed.v1"
        ],
        "model_roles": {
            "inspect_branch": "reasoning",
            "inspect_pr": "reasoning",
            "repair_reviews": "coding"
        },
        "contexts": {
            "entry": "isolated",
            "inspect_pr.compare.0.parallel.0": "isolated",
            "inspect_pr.compare.0.parallel.1": "isolated",
            "inspect_pr.refresh.0.goto": "isolated",
            "inspect_pr.repair.0.call": "kept"
        },
        "descriptors": {
            "github/context": descriptor.normalized_descriptor()
        }
    });
    assert_eq!(
        compiled.definition_hash,
        canonical_sha256(&identity).expect("identity is canonical JSON")
    );

    let mut changed_kind = identity.clone();
    changed_kind["definition_kind"] = json!("workflow");
    let mut changed_events = identity.clone();
    changed_events["events"][0] = json!("github.pull_request.closed.v1");
    let mut changed_roles = identity.clone();
    changed_roles["model_roles"]["repair_reviews"] = json!("reasoning");
    let mut changed_contexts = identity;
    changed_contexts["contexts"]["inspect_pr.repair.0.call"] = json!("isolated");

    for changed in [
        changed_kind,
        changed_events,
        changed_roles,
        changed_contexts,
    ] {
        assert_ne!(
            compiled.definition_hash,
            canonical_sha256(&changed).expect("changed identity is canonical JSON")
        );
    }
}

#[test]
fn rejects_agent_elapsed_limit_that_does_not_exceed_active_time() {
    let source = BABYSIT.replace("elapsed_seconds: 604800", "elapsed_seconds: 300");
    let diagnostics = compile_report(&source, &catalog([tool("github/context")]), &ceilings())
        .expect_err("elapsed time must exceed active execution time");

    assert_eq!(diagnostics[0].code, "invalid_elapsed_limit");
    assert_eq!(
        diagnostics[0].message,
        "elapsed_seconds must exceed active_execution_seconds"
    );
}

#[test]
fn agents_reuse_recursion_and_parallel_limit_checks() {
    let recursive = BABYSIT.replace(
        "on repaired(commit: string) { done commit }",
        "on repaired(commit: string) { again = call inspect_pr(repository = \"r\", pull_request = 1) context isolated done again }",
    );
    let recursive_diagnostics =
        compile_report(&recursive, &catalog([tool("github/context")]), &ceilings())
            .expect_err("agent calls cannot recurse");
    assert!(
        recursive_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("recursive call"))
    );

    let over_parallel = BABYSIT.replace("parallel_branches: 4", "parallel_branches: 1");
    let parallel_diagnostics = compile_report(
        &over_parallel,
        &catalog([tool("github/context")]),
        &ceilings(),
    )
    .expect_err("agent forks respect the parallel limit");
    assert!(
        parallel_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("parallel branch count"))
    );
}
