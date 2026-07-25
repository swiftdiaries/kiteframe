use kiteframe_contract::DiagnosticStage;
use kiteframe_core::{PackageLimits, parse_binding, parse_manifest};

const MINIMAL_MANIFEST: &str = r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: support, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text, tool-calling] }
"#;

const MINIMAL_BINDING: &str = r#"
apiVersion: kiteframe.dev/binding/v1alpha1
kind: RuntimeBinding
metadata: { runtime: local }
spec:
  models: { primary: local-model }
  capabilityProvider: local-capabilities
  auditSink: local-audit
"#;

#[test]
fn valid_manifest_and_binding_are_typed_after_preflight() {
    let manifest = parse_manifest(MINIMAL_MANIFEST.as_bytes(), PackageLimits::V1).unwrap();
    assert_eq!(manifest.metadata.name.as_str(), "support");

    let binding = parse_binding(MINIMAL_BINDING.as_bytes(), PackageLimits::V1).unwrap();
    assert_eq!(binding.metadata.runtime.as_str(), "local");
}

#[test]
fn duplicate_mapping_key_is_rejected_before_deserialization() {
    let yaml = include_bytes!("../../../tests/fixtures/packages/hostile/duplicate-key/agent.yaml");
    let errors = parse_manifest(yaml, PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert_eq!(errors[0].stage, DiagnosticStage::Parse);
    assert!(errors[0].message.as_str().contains("duplicate key"));
    let range = errors[0].source_range.expect("event marker source range");
    assert!(range.start > 0);
    assert!(range.end > range.start);
}

#[test]
fn quoted_duplicate_mapping_key_is_rejected() {
    let yaml = MINIMAL_MANIFEST.replace(
        "metadata: { name: support, version: 0.1.0 }",
        "metadata: { \"name\": support, 'name': other, version: 0.1.0 }",
    );
    let errors = parse_manifest(yaml.as_bytes(), PackageLimits::V1).unwrap_err();

    assert!(errors[0].message.as_str().contains("duplicate key"));
}

#[test]
fn yaml_over_one_mib_is_rejected_before_parsing() {
    let mut yaml = MINIMAL_MANIFEST.as_bytes().to_vec();
    yaml.extend(std::iter::repeat_n(
        b' ',
        PackageLimits::V1.max_yaml_bytes + 1 - yaml.len(),
    ));

    let errors = parse_manifest(&yaml, PackageLimits::V1).unwrap_err();
    assert!(errors[0].message.as_str().contains("byte limit"));
}

#[test]
fn yaml_at_one_mib_is_accepted() {
    let mut yaml = MINIMAL_MANIFEST.as_bytes().to_vec();
    yaml.extend(std::iter::repeat_n(
        b' ',
        PackageLimits::V1.max_yaml_bytes - yaml.len(),
    ));

    parse_manifest(&yaml, PackageLimits::V1).unwrap();
}

#[test]
fn nesting_over_32_is_rejected() {
    let yaml = format!("root: {}null{}", "[".repeat(33), "]".repeat(33));
    let errors = parse_manifest(yaml.as_bytes(), PackageLimits::V1).unwrap_err();

    assert!(errors[0].message.as_str().contains("nesting depth"));
    assert!(errors[0].source_range.is_some());
}

#[test]
fn more_than_ten_thousand_collection_entries_is_rejected() {
    let items = std::iter::repeat_n("null", 10_001)
        .collect::<Vec<_>>()
        .join(",");
    let yaml = format!("[{items}]");
    let errors = parse_manifest(yaml.as_bytes(), PackageLimits::V1).unwrap_err();

    assert!(errors[0].message.as_str().contains("collection entries"));
    assert!(errors[0].source_range.is_some());
}

#[test]
fn more_than_128_aliases_is_rejected() {
    let yaml = include_bytes!("../../../tests/fixtures/packages/hostile/alias-limit/agent.yaml");
    let errors = parse_manifest(yaml, PackageLimits::V1).unwrap_err();

    assert!(errors[0].message.as_str().contains("alias"));
    assert!(errors[0].source_range.is_some());
}

#[test]
fn unknown_manifest_fields_remain_closed_after_preflight() {
    let yaml = MINIMAL_MANIFEST.replace("  models:\n", "  surprise: true\n  models:\n");
    let errors = parse_manifest(yaml.as_bytes(), PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].stage, DiagnosticStage::Parse);
    assert!(errors[0].message.as_str().contains("unknown field"));
}

#[test]
fn unknown_binding_fields_remain_closed_after_preflight() {
    let yaml = MINIMAL_BINDING.replace(
        "  auditSink: local-audit\n",
        "  auditSink: local-audit\n  importPath: unsafe.module\n",
    );
    let errors = parse_binding(yaml.as_bytes(), PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].stage, DiagnosticStage::Parse);
    assert!(errors[0].message.as_str().contains("unknown field"));
}

#[test]
fn strict_yaml_panic_smoke_cases_return_errors() {
    let invalid_utf8 = [0xff, 0xfe, 0xfd];
    for yaml in [
        b"".as_slice(),
        invalid_utf8.as_slice(),
        b"root: [one, two".as_slice(),
        b"value: &value [*value]".as_slice(),
    ] {
        assert!(parse_manifest(yaml, PackageLimits::V1).is_err());
    }
}
