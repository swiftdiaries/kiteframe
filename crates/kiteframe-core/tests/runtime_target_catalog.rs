use std::fs;

use kiteframe_contract::{DiagnosticCategory, DiagnosticStage};
use kiteframe_core::load_runtime_target_catalog;

const FEATURE_CATALOG: &str = r#"{"components":{"middleware.first":{"features":["feature.alpha@1"],"kind":"middleware"},"middleware.second":{"features":["feature.alpha@1","feature.beta@2"],"kind":"middleware"}},"target":"deepagents"}"#;

#[test]
fn runtime_target_catalog_loader_owns_feature_union_and_digest() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("target.json");
    fs::write(&path, FEATURE_CATALOG).unwrap();

    let (target, components) = load_runtime_target_catalog(&path).unwrap();
    let features: Vec<_> = target
        .supported_features
        .iter()
        .map(|feature| feature.as_str())
        .collect();

    assert_eq!(target.target.as_str(), "deepagents");
    assert_eq!(components.target, target.target);
    assert_eq!(features, ["feature.alpha@1", "feature.beta@2"]);
    assert_eq!(
        target.target_digest.to_string(),
        "f91abdd19a699ace492c280e147b6182bdc9c27e8117827b61d2daf8add46f94"
    );
}

#[test]
fn runtime_target_catalog_loader_returns_redacted_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let invalid = directory.path().join("invalid.json");
    fs::write(
        &invalid,
        br#"{"target":"secret-value","components":"secret-value"}"#,
    )
    .unwrap();

    let invalid_errors = load_runtime_target_catalog(&invalid).unwrap_err();
    let missing_errors =
        load_runtime_target_catalog(&directory.path().join("missing.json")).unwrap_err();

    assert_eq!(invalid_errors[0].category, DiagnosticCategory::Runtime);
    assert_eq!(invalid_errors[0].stage, DiagnosticStage::Resolve);
    assert_eq!(
        invalid_errors[0].message.as_str(),
        "runtime target metadata is invalid"
    );
    assert!(!invalid_errors[0].message.as_str().contains("secret-value"));
    assert_eq!(
        missing_errors[0].message.as_str(),
        "runtime target metadata cannot be read"
    );
}
