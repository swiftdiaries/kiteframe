use kiteframe_contract::{AgentManifest, ContentCaptureRequirement, PackagePath, RuntimeBinding};

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
        "prompts/\0system.md",
        "prompts/\nsystem.md",
    ] {
        assert!(PackagePath::new(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn package_path_deserialization_enforces_constructor_invariants() {
    assert!(serde_yaml_ng::from_str::<PackagePath>("prompts/system.md").is_ok());
    assert!(serde_yaml_ng::from_str::<PackagePath>("../system.md").is_err());
}
