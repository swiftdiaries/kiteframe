use std::path::{Path, PathBuf};

use kiteframe_core::{PackageLimits, load_package};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/packages")
        .join(name)
}

#[test]
fn hostile_fixture_corpus_fails_closed() {
    for (name, code) in [
        ("duplicate-key", "KF-PKG-001"),
        ("alias-limit", "KF-PKG-001"),
        ("traversal", "KF-PKG-002"),
        ("case-collision", "KF-PKG-002"),
        ("package-tree-exact-collision", "KF-PKG-002"),
        ("package-tree-case-collision", "KF-PKG-002"),
        ("symlink", "KF-PKG-002"),
        ("non-utf8", "KF-PKG-002"),
        ("nested-cycle", "KF-PKG-001"),
    ] {
        let errors = load_package(
            fixture(&format!("hostile/{name}")).as_path(),
            PackageLimits::V1,
        )
        .unwrap_err();
        assert_eq!(errors[0].code.as_str(), code, "fixture {name}");
    }
}
