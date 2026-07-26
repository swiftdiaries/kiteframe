use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use kiteframe_contract::{
    CapabilityLock, ComponentMetadataCatalog, FeatureSet, PackageIdentity, PackagePath,
    RuntimeTargetDescriptor,
};
use kiteframe_core::{
    PackageLimits, canonical_json, hash_domain, load_package, load_runtime_binding,
};
use kiteframe_resolver::{ResolutionInput, resolve_agent};
use serde_json::{Value, json};

const TARGET_DIGEST_DOMAIN: &[u8] = b"runtime-target-catalog";

fn workspace_fixture(path: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(path)
}

fn resolution_fixture() -> ResolutionInput {
    let package_root = workspace_fixture("packages/support-agent");
    let package = load_package(&package_root, PackageLimits::V1).unwrap();
    let lock: CapabilityLock =
        serde_json::from_slice(&fs::read(package_root.join("capability.lock")).unwrap()).unwrap();
    let binding = load_runtime_binding(
        &package_root,
        &PackagePath::new("bindings/deepagents.yaml").unwrap(),
        PackageLimits::V1,
    )
    .unwrap();
    let components: ComponentMetadataCatalog = serde_json::from_slice(
        &fs::read(workspace_fixture("components/deepagents-test.json")).unwrap(),
    )
    .unwrap();
    let canonical_components = canonical_json(&components).unwrap();
    let supported_features: FeatureSet = components
        .components
        .values()
        .flat_map(|component| component.features.iter().cloned())
        .collect();
    let target = RuntimeTargetDescriptor {
        target: components.target.clone(),
        supported_features,
        target_digest: hash_domain(TARGET_DIGEST_DOMAIN, [canonical_components.as_slice()]),
    };

    ResolutionInput {
        package,
        lock,
        child_locks: BTreeMap::<PackageIdentity, CapabilityLock>::new(),
        binding,
        target,
        components,
    }
}

#[test]
fn resolved_support_agent_matches_checked_in_canonical_json() {
    let resolved = resolve_agent(resolution_fixture()).unwrap();
    let actual = canonical_json(&resolved).unwrap();
    let expected = include_bytes!("../../../tests/fixtures/resolved/support-agent.json");

    assert_eq!(actual.as_slice(), expected);
}

#[test]
fn support_agent_digest_record_matches_the_golden_ir() {
    let resolved = resolve_agent(resolution_fixture()).unwrap();
    let ir: Value = serde_json::from_slice(&canonical_json(&resolved).unwrap()).unwrap();
    let actual = canonical_json(&json!({
        "bindingDigest": ir["bindingDigest"],
        "lockDigest": ir["lockDigest"],
        "portableDigest": ir["portableDigest"],
        "resolvedDigest": ir["resolvedDigest"],
    }))
    .unwrap();
    let expected = include_bytes!("../../../tests/fixtures/resolved/support-agent.digests.json");

    assert_eq!(actual.as_slice(), expected);
}
