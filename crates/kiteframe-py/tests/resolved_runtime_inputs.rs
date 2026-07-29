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
        let components = projection.getattr("target_components").unwrap();
        let symbols = components
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .map(|component| {
                component
                    .getattr("symbol")
                    .expect("symbol property")
                    .extract::<String>()
                    .expect("symbol is text")
            })
            .collect::<Vec<_>>();
        assert!(symbols.contains(&"models.anthropic.sonnet".to_owned()));
        assert!(symbols.contains(&"capability-providers.primary".to_owned()));
        assert!(symbols.contains(&"audit-sinks.ledger".to_owned()));
    });
}
