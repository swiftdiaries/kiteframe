use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use kiteframe_contract::PackagePath;
use kiteframe_core::{
    PackageLimits, canonical_json, hash_domain, load_package, load_runtime_binding,
};
use proptest::prelude::*;

const EXPECTED_FORMAT_DIGEST: &str =
    "0c00ecfb35046a1908763a3372e2b73ed4cb5dce0bce1ee427d4a5b7363389fe";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/packages")
        .join(name)
}

#[test]
fn yaml_formatting_does_not_change_portable_digest() {
    let a = load_package(fixture("digest/format-a").as_path(), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/format-b").as_path(), PackageLimits::V1).unwrap();

    assert_eq!(a.portable_digest, b.portable_digest);
    assert_eq!(a.portable_digest.to_string(), EXPECTED_FORMAT_DIGEST);
}

#[test]
fn prompt_bytes_change_portable_digest() {
    let a = load_package(fixture("digest/prompt-a").as_path(), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/prompt-b").as_path(), PackageLimits::V1).unwrap();

    assert_ne!(a.portable_digest, b.portable_digest);
}

#[test]
fn skill_bytes_change_portable_digest() {
    let a = load_package(fixture("digest/skill-a").as_path(), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/skill-b").as_path(), PackageLimits::V1).unwrap();

    assert_ne!(a.portable_digest, b.portable_digest);
}

#[test]
fn binding_change_does_not_change_portable_digest() {
    let binding_path = PackagePath::new("bindings/runtime.yaml").unwrap();
    let binding_a = load_runtime_binding(
        fixture("digest/binding-a").as_path(),
        &binding_path,
        PackageLimits::V1,
    )
    .unwrap();
    let binding_b = load_runtime_binding(
        fixture("digest/binding-b").as_path(),
        &binding_path,
        PackageLimits::V1,
    )
    .unwrap();
    let a = load_package(fixture("digest/binding-a").as_path(), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/binding-b").as_path(), PackageLimits::V1).unwrap();

    assert_ne!(binding_a, binding_b, "fixture bindings must differ");
    assert_eq!(a.portable_digest, b.portable_digest);
}

#[test]
fn child_portable_digest_contributes_to_parent_digest() {
    let a = load_package(fixture("digest/child-a").as_path(), PackageLimits::V1).unwrap();
    let b = load_package(fixture("digest/child-b").as_path(), PackageLimits::V1).unwrap();

    assert_ne!(
        a.subagents.values().next().unwrap().portable_digest,
        b.subagents.values().next().unwrap().portable_digest
    );
    assert_ne!(a.portable_digest, b.portable_digest);
}

#[test]
fn domains_and_chunk_boundaries_are_distinct() {
    let first = hash_domain(b"first", [b"ab".as_slice(), b"c".as_slice()]);
    let changed_domain = hash_domain(b"second", [b"ab".as_slice(), b"c".as_slice()]);
    let changed_boundaries = hash_domain(b"first", [b"a".as_slice(), b"bc".as_slice()]);

    assert_ne!(first, changed_domain);
    assert_ne!(first, changed_boundaries);
    assert_eq!(first.to_string().len(), 64);
    assert!(
        first
            .to_string()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

proptest! {
    #[test]
    fn map_insertion_order_never_changes_canonical_bytes(
        entries in proptest::collection::vec(("[a-z]{1,8}", any::<u32>()), 1..32)
    ) {
        let forward: BTreeMap<_, _> = entries.into_iter().collect();
        let reverse: BTreeMap<_, _> = forward
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), *value))
            .collect();

        prop_assert_eq!(
            canonical_json(&forward).unwrap(),
            canonical_json(&reverse).unwrap()
        );
    }
}
