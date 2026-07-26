use std::path::{Path, PathBuf};

use kiteframe_contract::PackagePath;
use kiteframe_core::{PackageLimits, load_package, load_runtime_binding};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/packages")
        .join(name)
}

#[test]
fn unreferenced_file_does_not_enter_package() {
    let package = load_package(fixture("minimal").as_path(), PackageLimits::V1).unwrap();

    assert_eq!(package.prompt_assets().len(), 1);
    assert!(package.prompt_assets().contains_key("prompts/system.md"));
    assert!(!package.prompt_assets().contains_key("notes/private.txt"));
    assert!(package.skill_assets().is_empty());
}

#[test]
fn loaded_package_exposes_read_only_views_of_validated_content() {
    let package = load_package(fixture("minimal").as_path(), PackageLimits::V1).unwrap();

    assert!(package.root().as_path().ends_with("minimal"));
    assert_eq!(package.manifest().metadata.name.as_str(), "support");
    assert_eq!(package.prompt_assets().len(), 1);
    assert!(package.skill_assets().is_empty());
    assert!(package.subagents().is_empty());
    assert_eq!(package.portable_digest().to_string().len(), 64);
}

#[test]
fn explicitly_selected_runtime_binding_is_loaded_without_scanning_others() {
    let selected = PackagePath::new("bindings/deepagents.yaml").unwrap();
    let binding =
        load_runtime_binding(fixture("minimal").as_path(), &selected, PackageLimits::V1).unwrap();

    assert_eq!(binding.metadata.runtime.as_str(), "deepagents");
    load_package(fixture("minimal").as_path(), PackageLimits::V1).unwrap();
}

#[test]
fn nested_agent_is_loaded_from_its_declared_manifest() {
    let package = load_package(fixture("nested").as_path(), PackageLimits::V1).unwrap();
    let child = &package.subagents()["agents/escalation/agent.yaml"];

    assert_eq!(child.manifest().metadata.name.as_str(), "escalation");
    assert!(child.prompt_assets().contains_key("prompts/system.md"));
}

#[test]
fn nested_manifest_must_be_named_agent_yaml() {
    let errors = load_package(
        fixture("hostile/nested-non-agent-manifest").as_path(),
        PackageLimits::V1,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("nested agent manifest"));
}

#[test]
fn nested_reference_case_colliding_with_implicit_manifest_is_rejected() {
    let errors = load_package(
        fixture("hostile/nested-manifest-case-collision").as_path(),
        PackageLimits::V1,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
}

#[test]
fn parent_traversal_is_rejected() {
    let errors =
        load_package(fixture("hostile/traversal").as_path(), PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
}

#[test]
fn absolute_path_is_rejected() {
    let errors =
        load_package(fixture("hostile/absolute").as_path(), PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
}

#[test]
fn case_colliding_references_are_rejected() {
    let errors = load_package(
        fixture("hostile/case-collision").as_path(),
        PackageLimits::V1,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
}

#[test]
fn reference_case_colliding_with_manifest_is_rejected() {
    let errors = load_package(
        fixture("hostile/manifest-case-collision").as_path(),
        PackageLimits::V1,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
}

#[test]
fn exact_path_overlap_across_parent_and_child_packages_is_rejected() {
    let errors = load_package(
        fixture("hostile/package-tree-exact-collision").as_path(),
        PackageLimits::V1,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
    assert!(errors[0].message.as_str().contains("package tree"));
}

#[test]
fn case_only_path_overlap_across_parent_and_child_packages_is_rejected() {
    let errors = load_package(
        fixture("hostile/package-tree-case-collision").as_path(),
        PackageLimits::V1,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
    assert!(errors[0].message.as_str().contains("package tree"));
}

#[test]
fn missing_referenced_file_is_rejected() {
    let errors =
        load_package(fixture("hostile/missing-file").as_path(), PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
}

#[test]
fn non_utf8_referenced_asset_is_rejected() {
    let errors =
        load_package(fixture("hostile/non-utf8").as_path(), PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
}

#[test]
fn nested_identity_cycle_is_rejected() {
    let errors =
        load_package(fixture("hostile/nested-cycle").as_path(), PackageLimits::V1).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("cycle"));
}

#[test]
fn duplicate_sibling_identity_is_rejected() {
    let errors = load_package(
        fixture("hostile/duplicate-identity").as_path(),
        PackageLimits::V1,
    )
    .unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("duplicate"));
}

#[test]
fn referenced_asset_over_per_asset_budget_is_rejected() {
    let mut limits = PackageLimits::V1;
    limits.max_text_asset_bytes = 4;
    let errors = load_package(fixture("hostile/byte-budget").as_path(), limits).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("text asset byte limit"));
}

#[test]
fn total_referenced_bytes_are_bounded() {
    let root = fixture("hostile/byte-budget");
    let referenced_bytes = std::fs::read(root.join("agent.yaml")).unwrap().len()
        + std::fs::read(root.join("prompts/system.md")).unwrap().len();
    let mut limits = PackageLimits::V1;
    limits.max_total_referenced_bytes = referenced_bytes - 1;
    let errors = load_package(root.as_path(), limits).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(
        errors[0]
            .message
            .as_str()
            .contains("total referenced byte limit")
    );
}

#[test]
fn total_referenced_byte_budget_is_shared_with_nested_packages() {
    let root = fixture("hostile/nested-byte-budget");
    let child_root = root.join("agents/child");
    let root_bytes = std::fs::read(root.join("agent.yaml")).unwrap().len()
        + std::fs::read(root.join("prompts/system.md")).unwrap().len();
    let child_bytes = std::fs::read(child_root.join("agent.yaml")).unwrap().len()
        + std::fs::read(child_root.join("prompts/system.md"))
            .unwrap()
            .len();
    let combined_boundary = root_bytes + child_bytes - 1;
    assert!(root_bytes <= combined_boundary);
    assert!(child_bytes <= combined_boundary);

    let mut limits = PackageLimits::V1;
    limits.max_total_referenced_bytes = combined_boundary;
    load_package(child_root.as_path(), limits).expect("child inputs fit independently");
    let errors = load_package(root.as_path(), limits).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(
        errors[0]
            .message
            .as_str()
            .contains("total referenced byte limit")
    );
}

#[test]
fn nesting_beyond_configured_limit_is_rejected() {
    let mut limits = PackageLimits::V1;
    limits.max_subagent_depth = 0;
    let errors = load_package(fixture("nested").as_path(), limits).unwrap_err();

    assert_eq!(errors[0].code.as_str(), "KF-PKG-001");
    assert!(errors[0].message.as_str().contains("nesting"));
}

#[test]
fn v1_containment_limits_are_fixed() {
    assert_eq!(PackageLimits::V1.max_text_asset_bytes, 4 * 1024 * 1024);
    assert_eq!(
        PackageLimits::V1.max_total_referenced_bytes,
        32 * 1024 * 1024
    );
    assert_eq!(PackageLimits::V1.max_subagent_depth, 16);
}

#[test]
fn referenced_symlink_is_rejected() {
    #[cfg(unix)]
    {
        let errors =
            load_package(fixture("hostile/symlink").as_path(), PackageLimits::V1).unwrap_err();
        assert_eq!(errors[0].code.as_str(), "KF-PKG-002");
    }

    #[cfg(not(unix))]
    eprintln!("symlink fixture is unsupported on this target");
}
