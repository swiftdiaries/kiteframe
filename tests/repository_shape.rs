#[test]
fn repository_contains_one_runtime() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for removed in [
        "crates",
        "python",
        "fuzz",
        "openfga",
        "runtime-targets",
        "schemas",
        "scripts",
    ] {
        assert!(!root.join(removed).exists(), "obsolete path: {removed}");
    }
    assert!(root.join("src/main.rs").is_file());
}
