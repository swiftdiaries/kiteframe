use std::collections::BTreeSet;

use kiteframe::dsl::{
    CompiledStatement, ContextMode, DefinitionKind, DiagnosticSeverity, Limits, compile,
    compile_report,
};
use kiteframe::{
    CatalogTool, ClosedSchema, EffectKind, RecoverySupport, SessionId, ToolCatalogSnapshot,
};
use serde_json::json;
use uuid::Uuid;

const SUPPORT: &str = include_str!("fixtures/dsl/support.kite");

fn ceilings() -> Limits {
    Limits {
        steps: 100,
        model_turns_per_task: 12,
        tool_calls_per_response: 8,
        parallel_branches: 4,
        response_item_bytes: 16 * 1024,
        active_execution_seconds: 300,
        elapsed_seconds: u64::MAX,
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
fn compiles_the_support_workflow_with_strict_completion_and_immutable_bindings() {
    let catalog = catalog([
        tool("docs/search_docs"),
        tool("logs/search_logs"),
        tool("metrics/search_metrics"),
    ]);

    let report = compile_report(SUPPORT, &catalog, &ceilings()).expect("support compiles");
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(report.diagnostics[0].code, "deprecated_workflow");
    let compiled = report.definition;

    assert_eq!(compiled.dsl_version, 1);
    assert_eq!(compiled.definition_kind, DefinitionKind::Workflow);
    assert_eq!(compiled.name, "support");
    assert_eq!(compiled.entry.context, ContextMode::Legacy);
    assert!(compiled.events.is_empty());
    assert!(
        compiled
            .tasks
            .values()
            .all(|task| task.model_role.is_none())
    );
    assert_eq!(compiled.tool_bindings.len(), 3);
    assert_eq!(
        compiled
            .tool_bindings
            .iter()
            .map(|binding| binding.qualified_name.as_str())
            .collect::<Vec<_>>(),
        [
            "docs/search_docs",
            "logs/search_logs",
            "metrics/search_metrics"
        ]
    );
    assert!(
        compiled
            .tool_bindings
            .iter()
            .all(|binding| binding.catalog_epoch_id == *catalog.epoch_id()
                && binding.descriptor_digest == binding.descriptor.descriptor_digest())
    );

    let triage = compiled.tasks.get("triage").expect("triage is compiled");
    let completion = &triage.completion;
    assert!(completion.outcomes.contains_key("resolved"));
    assert!(completion.required_nullable_fields.contains_key("answer"));
    assert!(completion.required_nullable_fields.contains_key("query"));
    assert_eq!(triage.tools[0].qualified_name, "docs/search_docs");
    let CompiledStatement::Parallel { calls } = &triage.outcomes["investigate"].statements[0]
    else {
        panic!("investigation starts with parallel calls");
    };
    assert!(
        calls
            .iter()
            .all(|binding| binding.call.context == ContextMode::Legacy)
    );
}

#[test]
fn rejects_missing_or_ambiguous_visible_tool_bindings() {
    let source = r#"
workflow w() -> string { start t() }
task t() -> string { prompt """x""" tools [search] on ok() { done "x" } }
"#;

    let unknown = compile(source, &catalog([]), &ceilings()).expect_err("hidden tools fail closed");
    assert!(
        unknown
            .iter()
            .any(|error| error.message.contains("visible tool"))
    );

    let ambiguous = compile(
        source,
        &catalog([tool("docs/search"), tool("other/search")]),
        &ceilings(),
    )
    .expect_err("unqualified source names must not select an arbitrary server");
    assert!(
        ambiguous
            .iter()
            .any(|error| error.message.contains("ambiguous"))
    );
}

#[test]
fn rejects_invalid_completion_unbound_values_and_type_mismatches() {
    let source = r#"
workflow w() -> string { start t() }
task t() -> string {
  prompt """x"""
  tools []
  on one(value: string) { done value }
  on two(value: integer) { done missing }
}
"#;

    let errors = compile(source, &catalog([]), &ceilings()).expect_err("invalid completions fail");
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("compatible types"))
    );
    assert!(errors.iter().any(|error| error.message.contains("unbound")));
}

#[test]
fn rejects_recursive_call_chains_but_allows_bounded_goto_cycles() {
    let recursive = r#"
workflow w() -> string { start a() limits { steps: 1 } }
task a() -> string { prompt """x""" tools [] on ok() { child = call b() done child } }
task b() -> string { prompt """x""" tools [] on ok() { child = call a() done child } }
"#;
    let errors =
        compile(recursive, &catalog([]), &ceilings()).expect_err("call recursion grows the stack");
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("recursive call"))
    );

    let goto_cycle = r#"
workflow w() -> string { start a() limits { steps: 1 } }
task a() -> string { prompt """x""" tools [] on again() { goto a() } }
"#;
    assert!(compile(goto_cycle, &catalog([]), &ceilings()).is_ok());
}

#[test]
fn rejects_unreachable_tasks_and_deployment_limit_excesses() {
    let unreachable = r#"
workflow w() -> string { start a() }
task a() -> string { prompt """x""" tools [] on ok() { done "x" } }
task unused() -> string { prompt """x""" tools [] on ok() { done "x" } }
"#;
    let errors =
        compile(unreachable, &catalog([]), &ceilings()).expect_err("dead tasks are rejected");
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("unreachable"))
    );

    let exceeds = r#"
workflow w() -> string { start a() limits { steps: 101 } }
task a() -> string { prompt """x""" tools [] on ok() { done "x" } }
"#;
    let errors = compile(exceeds, &catalog([]), &ceilings()).expect_err("ceilings are enforced");
    assert!(errors.iter().any(|error| error.message.contains("ceiling")));

    let over_parallel_budget = r#"
workflow w() -> string { start a() limits { parallel_branches: 1 } }
task a() -> string { prompt """x""" tools [] on ok() { parallel { first = call b() second = call b() } done "x" } }
task b() -> string { prompt """x""" tools [] on ok() { done "x" } }
"#;
    let errors = compile(over_parallel_budget, &catalog([]), &ceilings())
        .expect_err("parallel declarations cannot exceed their workflow budget");
    assert!(
        errors
            .iter()
            .any(|error| error.message.contains("parallel branch count"))
    );
}

#[test]
fn definition_identity_is_stable_for_identical_source_and_exact_descriptors() {
    let source = r#"
workflow w() -> string { start t() }
task t() -> string { prompt """x""" tools [search] on ok() { done "x" } }
"#;
    let first_catalog = catalog([tool("docs/search")]);
    let second_catalog = catalog([tool("docs/search")]);

    let first = compile(source, &first_catalog, &ceilings()).expect("first compile succeeds");
    let second = compile(source, &second_catalog, &ceilings()).expect("second compile succeeds");

    assert_eq!(first.definition_hash, second.definition_hash);
    assert_eq!(first.source, source);
}

#[test]
fn freezes_the_support_compilation_output() {
    let catalog = catalog([
        tool("docs/search_docs"),
        tool("logs/search_logs"),
        tool("metrics/search_metrics"),
    ]);
    let compiled = compile(SUPPORT, &catalog, &ceilings()).expect("support compiles");

    let actual = json!({
        "definition_hash": compiled.definition_hash,
        "entry": compiled.entry,
        "tool_bindings": compiled.tool_bindings.iter().map(|binding| json!({
            "qualified_name": binding.qualified_name,
            "catalog_epoch_id": binding.catalog_epoch_id,
            "descriptor_digest": binding.descriptor_digest,
        })).collect::<Vec<_>>(),
    });
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/dsl/support.compiled.json"))
            .expect("golden fixture is valid JSON");

    assert_eq!(actual, expected);
}
