use _native::PyResolvedRuntimeInputs;
use kiteframe_contract::{PackagePath, ResolvedAgent};
use kiteframe_core::{PackageLimits, load_runtime_binding, load_runtime_target_catalog};
use pyo3::prelude::*;
use std::path::PathBuf;

#[test]
fn frozen_projection_retains_validated_binding_target_and_component_values() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let package = workspace.join("tests/fixtures/packages/support-agent");
    let binding = load_runtime_binding(
        &package,
        &PackagePath::new("bindings/deepagents.yaml").expect("valid fixture path"),
        PackageLimits::V1,
    )
    .expect("fixture binding validates");
    let (target, components) = load_runtime_target_catalog(
        &workspace.join("tests/fixtures/components/deepagents-test.json"),
    )
    .expect("fixture catalog validates");
    let resolved: ResolvedAgent = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/resolved/support-agent.json"
    ))
    .expect("fixture IR validates");

    Python::attach(|py| {
        let projection = Py::new(
            py,
            PyResolvedRuntimeInputs::new(resolved, binding, target.target, components.components),
        )
        .expect("runtime inputs project");
        let projection = projection.bind(py);
        assert_eq!(
            projection
                .getattr("runtime_target")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "deepagents"
        );
        let binding = projection.getattr("runtime_binding").unwrap();
        assert_eq!(
            binding
                .getattr("runtime")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "deepagents"
        );
        assert_eq!(
            binding
                .getattr("capability_provider")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "capability-providers.primary"
        );
        assert_eq!(
            binding
                .getattr("audit_sink")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "audit-sinks.ledger"
        );
        assert_eq!(
            binding
                .getattr("harness_profile")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "profiles.deepagents"
        );
        let components = projection.getattr("target_components").unwrap();
        let component_values = components
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .map(|component| {
                (
                    component
                        .getattr("symbol")
                        .expect("symbol property")
                        .extract::<String>()
                        .expect("symbol is text"),
                    component
                        .getattr("kind")
                        .expect("kind property")
                        .extract::<String>()
                        .expect("kind is text"),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            component_values.contains(&("models.anthropic.sonnet".to_owned(), "model".to_owned()))
        );
        assert!(component_values.contains(&(
            "capability-providers.primary".to_owned(),
            "capability_provider".to_owned()
        )));
        assert!(
            component_values.contains(&("audit-sinks.ledger".to_owned(), "audit_sink".to_owned()))
        );
        assert!(component_values.contains(&(
            "profiles.deepagents".to_owned(),
            "harness_profile".to_owned()
        )));

        let report = projection.getattr("compilation_report").unwrap();
        assert_eq!(
            report.get_type().name().expect("report type has a name"),
            "CompilationReport"
        );
        assert!(
            report
                .getattr("warnings")
                .unwrap()
                .extract::<Vec<(String, String)>>()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            report
                .getattr("decisions")
                .unwrap()
                .extract::<Vec<(String, String)>>()
                .unwrap(),
            vec![
                (
                    "features".to_owned(),
                    "0 required and 0 optional enabled".to_owned(),
                ),
                ("models".to_owned(), "1 roles resolved".to_owned()),
            ]
        );
    });
}
