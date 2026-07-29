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

    let projection =
        PyResolvedRuntimeInputs::new(resolved, binding, target.target, components.components);

    assert_eq!(projection.runtime_target(), "deepagents");
    assert_eq!(projection.runtime_binding().runtime(), "deepagents");
    assert_eq!(
        projection.runtime_binding().capability_provider(),
        "capability-providers.primary"
    );
    assert_eq!(
        projection.runtime_binding().audit_sink(),
        "audit-sinks.ledger"
    );
    Python::attach(|py| {
        let components = projection
            .target_components(py)
            .expect("component projections build");
        let symbols = components
            .iter()
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
