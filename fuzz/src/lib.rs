#![forbid(unsafe_code)]

use kiteframe_core::{PackageLimits, parse_manifest};

const BYTE_LIMIT_SEED: &[u8] = include_bytes!("../seeds/strict_yaml/byte-limit");
const NESTING_LIMIT_SEED: &[u8] = include_bytes!("../seeds/strict_yaml/nesting-limit");
const COLLECTION_LIMIT_SEED: &[u8] = include_bytes!("../seeds/strict_yaml/collection-limit");
const ALIAS_LIMIT_SEED: &[u8] = include_bytes!("../seeds/strict_yaml/alias-limit");

/// Exercises arbitrary parser input and asserts observable failures for tagged
/// inputs that are guaranteed to violate one configured parser limit.
pub fn exercise_strict_yaml_input(bytes: &[u8]) {
    match bytes.first().copied() {
        Some(b'B') => {
            let input = tagged_payload(BYTE_LIMIT_SEED);
            let mut limits = PackageLimits::V1;
            limits.max_yaml_bytes = input.len() - 1;
            assert_limit_rejected(input, limits, "byte limit");
        }
        Some(b'D') => {
            let mut limits = PackageLimits::V1;
            limits.max_nesting_depth = 1;
            assert_limit_rejected(tagged_payload(NESTING_LIMIT_SEED), limits, "nesting depth");
        }
        Some(b'C') => {
            let mut limits = PackageLimits::V1;
            limits.max_collection_entries = 0;
            assert_limit_rejected(
                tagged_payload(COLLECTION_LIMIT_SEED),
                limits,
                "collection entries",
            );
        }
        Some(b'A') => {
            let mut limits = PackageLimits::V1;
            limits.max_aliases = 0;
            assert_limit_rejected(tagged_payload(ALIAS_LIMIT_SEED), limits, "alias limit");
        }
        _ => {
            let _result = parse_manifest(bytes, PackageLimits::V1);
        }
    }
}

fn tagged_payload(seed: &'static [u8]) -> &'static [u8] {
    seed.get(1..)
        .expect("checked-in invariant seeds contain a one-byte mode tag")
}

fn assert_limit_rejected(input: &[u8], limits: PackageLimits, expected_message: &str) {
    let diagnostics =
        parse_manifest(input, limits).expect_err("known limit violation parsed successfully");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.as_str().contains(expected_message)),
        "known limit violation returned the wrong diagnostics: {diagnostics:?}"
    );
}
