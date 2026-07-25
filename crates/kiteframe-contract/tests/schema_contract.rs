use kiteframe_contract::{
    AgentManifest, CapabilityVersion, ContentCaptureRequirement, PackagePath, PackageVersion,
    ResourceSelector, RuntimeBinding,
};
use schemars::JsonSchema;

const MINIMAL_MANIFEST: &str = r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: support, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text, tool-calling], minContextTokens: 64000 }
"#;

#[test]
fn manifest_rejects_unknown_fields() {
    let yaml = r#"
apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: support, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text, tool-calling], minContextTokens: 64000 }
  surprise: true
"#;
    assert!(serde_yaml_ng::from_str::<AgentManifest>(yaml).is_err());
}

#[test]
fn manifest_rejects_unknown_nested_fields_and_values() {
    let unknown_field = MINIMAL_MANIFEST.replace(
        "minContextTokens: 64000",
        "minContextTokens: 64000, endpoint: https://example.test",
    );
    assert!(serde_yaml_ng::from_str::<AgentManifest>(&unknown_field).is_err());

    let unknown_value = MINIMAL_MANIFEST.replace("text, tool-calling", "text, vision");
    assert!(serde_yaml_ng::from_str::<AgentManifest>(&unknown_value).is_err());
}

#[test]
fn manifest_requires_exact_version_and_kind_literals() {
    let wrong_version =
        MINIMAL_MANIFEST.replace("kiteframe.dev/v1alpha1", "kiteframe.dev/v1alpha2");
    assert!(serde_yaml_ng::from_str::<AgentManifest>(&wrong_version).is_err());

    let wrong_kind = MINIMAL_MANIFEST.replace("kind: Agent", "kind: Workflow");
    assert!(serde_yaml_ng::from_str::<AgentManifest>(&wrong_kind).is_err());
}

#[test]
fn content_capture_permission_defaults_to_disabled() {
    let manifest = serde_yaml_ng::from_str::<AgentManifest>(MINIMAL_MANIFEST).unwrap();
    assert_eq!(
        manifest.spec.observability.content_capture,
        ContentCaptureRequirement::default()
    );
    assert!(!manifest.spec.observability.content_capture.allowed);
    assert!(
        manifest
            .spec
            .observability
            .content_capture
            .classifications
            .is_empty()
    );
}

#[test]
fn binding_contains_symbols_but_no_executable_or_secret_fields() {
    let schema = schemars::schema_for!(RuntimeBinding);
    let text = serde_json::to_string(&schema).unwrap();
    assert!(text.contains("capabilityProvider"));
    assert!(!text.contains("importPath"));
    assert!(!text.contains("credentials"));
    assert!(!text.contains("endpoint"));
    assert!(!text.contains("capturedContent"));
    assert!(!text.contains("portableGrant"));
}

#[test]
fn package_path_accepts_only_normalized_relative_slash_separated_paths() {
    let path = PackagePath::new("skills/case-summary/SKILL.md").unwrap();
    assert_eq!(path.as_str(), "skills/case-summary/SKILL.md");

    for invalid in [
        "",
        "/prompts/system.md",
        "prompts//system.md",
        "prompts/./system.md",
        "prompts/../system.md",
        "../system.md",
        r"prompts\system.md",
        "C:/prompts/system.md",
        "C:prompts/system.md",
        "prompts/\0system.md",
        "prompts/\nsystem.md",
        "prompts/\u{85}system.md",
    ] {
        assert!(PackagePath::new(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn package_path_deserialization_enforces_constructor_invariants() {
    assert!(serde_yaml_ng::from_str::<PackagePath>("prompts/system.md").is_ok());
    assert!(serde_yaml_ng::from_str::<PackagePath>("../system.md").is_err());
}

#[test]
fn package_versions_reject_malformed_semantic_versions() {
    for valid in ["0.1.0", "1.2.3-alpha.1+build.5"] {
        assert!(PackageVersion::new(valid).is_ok(), "{valid:?}");
    }
    for invalid in [
        "1foo",
        "1..2",
        "1+",
        "1.2",
        "01.2.3",
        "1.2.3-",
        "1.2.3+",
        "18446744073709551616.0.0",
    ] {
        assert!(PackageVersion::new(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn capability_versions_accept_only_v1_caret_constraints() {
    for valid in ["^1.2", "^1.2.3", "^0.1"] {
        assert!(CapabilityVersion::new(valid).is_ok(), "{valid:?}");
    }
    for invalid in [
        "^1.",
        "^1.2+",
        "^1..2",
        "^01.2",
        "1.2",
        ">=1.2",
        "^1.2.3-rc.1",
        "^18446744073709551616.0",
    ] {
        assert!(CapabilityVersion::new(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn generated_newtype_patterns_match_rust_validation() {
    assert_pattern_parity::<PackageVersion>(
        &[
            ("0.1.0", true),
            ("1.2.3-alpha.1+build.5", true),
            ("1foo", false),
            ("1..2", false),
            ("1+", false),
            ("1.2.3-", false),
            ("18446744073709551614.0.0", true),
            ("18446744073709551615.0.0", true),
            ("18446744073709551616.0.0", false),
            ("99999999999999999999.0.0", false),
        ],
        |value| PackageVersion::new(value).is_ok(),
    );
    assert_pattern_parity::<CapabilityVersion>(
        &[
            ("^1.2", true),
            ("^1.2.3", true),
            ("^1.", false),
            ("^1.2+", false),
            (">=1.2", false),
            ("^18446744073709551614.0", true),
            ("^18446744073709551615.0", true),
            ("^18446744073709551616.0", false),
            ("^99999999999999999999.0", false),
        ],
        |value| CapabilityVersion::new(value).is_ok(),
    );
    assert_pattern_parity::<PackagePath>(
        &[
            ("prompts/system.md", true),
            ("../system.md", false),
            ("C:/system.md", false),
            ("C:system.md", false),
            ("prompts/\u{85}system.md", false),
        ],
        |value| PackagePath::new(value).is_ok(),
    );
    assert_pattern_parity::<ResourceSelector>(
        &[
            ("tenant:${context.tenant_id}/case:*", true),
            ("tenant:\ncase", false),
            ("tenant:\u{85}case", false),
        ],
        |value| ResourceSelector::new(value).is_ok(),
    );
}

fn assert_pattern_parity<T: JsonSchema>(
    cases: &[(&str, bool)],
    rust_accepts: impl Fn(&str) -> bool,
) {
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    let pattern = schema["pattern"].as_str().unwrap();
    let regex = fancy_regex::Regex::new(pattern).unwrap();
    for &(value, expected) in cases {
        let schema_accepts = regex.is_match(value).unwrap();
        assert_eq!(rust_accepts(value), expected, "Rust parity for {value:?}");
        assert_eq!(schema_accepts, expected, "schema parity for {value:?}");
    }
}
