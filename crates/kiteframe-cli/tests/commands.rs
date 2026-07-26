use std::{
    fs,
    path::{Path, PathBuf},
};

use assert_cmd::{Command, cargo::cargo_bin_cmd};
use serde_json::{Value, json};

fn command() -> Command {
    cargo_bin_cmd!("kiteframe")
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/packages")
        .join(name)
        .display()
        .to_string()
}

fn workspace_fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn parse_single_json(stdout: &[u8]) -> Value {
    let mut stream = serde_json::Deserializer::from_slice(stdout).into_iter::<Value>();
    let value = stream.next().expect("one JSON value").expect("valid JSON");
    assert!(
        stream.next().is_none(),
        "stdout contained more than one JSON value"
    );
    value
}

fn copy_package(source: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("package");
    fs::create_dir_all(destination.join("bindings")).unwrap();
    fs::create_dir_all(destination.join("prompts")).unwrap();
    for relative in [
        "agent.yaml",
        "capability.lock",
        "bindings/deepagents.yaml",
        "prompts/system.md",
    ] {
        fs::copy(
            Path::new(&fixture(source)).join(relative),
            destination.join(relative),
        )
        .unwrap();
    }
    (directory, destination)
}

#[test]
fn check_locked_does_not_require_catalog_access() {
    let output = command()
        .args(["check", &fixture("support-agent"), "--locked", "--json"])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .assert()
        .success();

    assert!(output.get_output().stderr.is_empty());
    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["status"], "valid");
}

#[test]
fn compile_emits_canonical_ir_not_runtime_graph() {
    let output = command()
        .args([
            "compile",
            &fixture("support-agent"),
            "--binding",
            &fixture("support-agent/bindings/deepagents.yaml"),
            "--target",
            &workspace_fixture("components/deepagents-test.json"),
            "--locked",
            "--json",
        ])
        .assert()
        .success();

    assert!(output.get_output().stderr.is_empty());
    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["schemaVersion"], "kiteframe.dev/ir/v1alpha1");
    assert!(body.get("compiledGraph").is_none());
}

#[test]
fn compile_writes_canonical_ir_to_an_explicit_output_path() {
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("support-agent.json");
    let output = command()
        .args([
            "compile",
            &fixture("support-agent"),
            "--binding",
            &fixture("support-agent/bindings/deepagents.yaml"),
            "--target",
            &workspace_fixture("components/deepagents-test.json"),
            "--locked",
            "--json",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    assert!(output.get_output().stdout.is_empty());
    assert!(output.get_output().stderr.is_empty());
    let bytes = fs::read(output_path).unwrap();
    assert_eq!(
        bytes,
        include_bytes!("../../../tests/fixtures/resolved/support-agent.json")
    );
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let digests = serde_json_canonicalizer::to_vec(&json!({
        "bindingDigest": body["bindingDigest"],
        "lockDigest": body["lockDigest"],
        "portableDigest": body["portableDigest"],
        "resolvedDigest": body["resolvedDigest"],
    }))
    .unwrap();
    assert_eq!(
        digests,
        include_bytes!("../../../tests/fixtures/resolved/support-agent.digests.json")
    );
}

fn assert_compile_output_rejected(package: &Path, target: &Path, output: &Path) {
    let result = command()
        .args([
            "compile",
            package.to_str().unwrap(),
            "--binding",
            package.join("bindings/deepagents.yaml").to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
            "--locked",
            "--json",
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .code(5);

    assert!(result.get_output().stderr.is_empty());
    let body = parse_single_json(&result.get_output().stdout);
    assert_eq!(body["diagnostics"][0]["code"], "KF-RUNTIME-002");
    assert_eq!(
        body["diagnostics"][0]["message"],
        "compiled IR output path overlaps protected input"
    );
}

#[test]
fn compile_rejects_outputs_inside_the_package_and_preserves_consumed_inputs() {
    for protected_relative in ["agent.yaml", "capability.lock", "bindings/deepagents.yaml"] {
        let (_directory, package) = copy_package("support-agent");
        let protected = package.join(protected_relative);
        let before = fs::read(&protected).unwrap();

        assert_compile_output_rejected(
            &package,
            Path::new(&workspace_fixture("components/deepagents-test.json")),
            &protected,
        );

        assert_eq!(fs::read(protected).unwrap(), before);
    }

    let (_directory, package) = copy_package("support-agent");
    let new_package_artifact = package.join("resolved.json");
    assert_compile_output_rejected(
        &package,
        Path::new(&workspace_fixture("components/deepagents-test.json")),
        &new_package_artifact,
    );
    assert!(!new_package_artifact.exists());
}

#[test]
fn compile_rejects_an_output_that_overlaps_the_target_input() {
    let (directory, package) = copy_package("support-agent");
    let target = directory.path().join("target.json");
    fs::copy(
        workspace_fixture("components/deepagents-test.json"),
        &target,
    )
    .unwrap();
    let before = fs::read(&target).unwrap();

    assert_compile_output_rejected(&package, &target, &target);

    assert_eq!(fs::read(target).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn compile_rejects_symlink_aliases_to_package_and_target_inputs() {
    use std::os::unix::fs::symlink;

    let (directory, package) = copy_package("support-agent");
    let target = directory.path().join("target.json");
    fs::copy(
        workspace_fixture("components/deepagents-test.json"),
        &target,
    )
    .unwrap();
    let package_alias = directory.path().join("package-alias");
    symlink(&package, &package_alias).unwrap();
    let package_alias_output = package_alias.join("resolved.json");
    assert_compile_output_rejected(&package, &target, &package_alias_output);
    assert!(!package.join("resolved.json").exists());

    let target_alias = directory.path().join("target-alias.json");
    symlink(&target, &target_alias).unwrap();
    let before = fs::read(&target).unwrap();
    assert_compile_output_rejected(&package, &target, &target_alias);
    assert_eq!(fs::read(target).unwrap(), before);
}

#[test]
fn compile_output_write_failures_are_redacted_structured_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let sentinels = [
        "credential-LEAK",
        "token-LEAK",
        "argument-LEAK",
        "result-LEAK",
    ];
    let sensitive = sentinels.join("_");
    let output_path = directory.path().join("missing").join(sensitive);
    let base_args = [
        "compile",
        &fixture("support-agent"),
        "--binding",
        &fixture("support-agent/bindings/deepagents.yaml"),
        "--target",
        &workspace_fixture("components/deepagents-test.json"),
        "--locked",
        "--output",
        output_path.to_str().unwrap(),
    ];

    let json_output = command()
        .args(base_args)
        .arg("--json")
        .assert()
        .failure()
        .code(5);
    assert!(json_output.get_output().stderr.is_empty());
    let body = parse_single_json(&json_output.get_output().stdout);
    assert_eq!(body["diagnostics"][0]["code"], "KF-RUNTIME-002");
    assert_eq!(
        body["diagnostics"][0]["message"],
        "compiled IR output cannot be written"
    );
    let json = String::from_utf8_lossy(&json_output.get_output().stdout);
    for sentinel in sentinels {
        assert!(
            !json.contains(sentinel),
            "JSON diagnostic leaked {sentinel}"
        );
    }

    let human_output = command().args(base_args).assert().failure().code(5);
    assert!(human_output.get_output().stdout.is_empty());
    let stderr = String::from_utf8_lossy(&human_output.get_output().stderr);
    assert_eq!(
        stderr,
        "KF-RUNTIME-002 Error: compiled IR output cannot be written\n"
    );
    for sentinel in sentinels {
        assert!(
            !stderr.contains(sentinel),
            "human diagnostic leaked {sentinel}"
        );
    }
}

#[test]
fn explain_lists_symbols_without_exposing_package_content() {
    let checked = command()
        .args(["check", &fixture("support-agent"), "--json"])
        .assert()
        .success();
    let checked_body = parse_single_json(&checked.get_output().stdout);

    let output = command()
        .args([
            "explain",
            &fixture("support-agent"),
            "--binding",
            &fixture("support-agent/bindings/deepagents.yaml"),
            "--target",
            &workspace_fixture("components/deepagents-test.json"),
            "--locked",
            "--json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let body = parse_single_json(stdout.as_bytes());
    assert_eq!(body["status"], "resolved");
    assert_eq!(body["portableDigest"], checked_body["portableDigest"]);
    assert_eq!(
        body["models"]["primary"],
        Value::String("models.anthropic.sonnet".to_owned())
    );
    assert_eq!(
        body["capabilities"][0]["identity"]["version"],
        Value::String("1.2.0".to_owned())
    );
    assert!(!stdout.contains("Help support agents read cases safely."));
    assert!(!stdout.contains("\"prompts\""));
    assert!(!stdout.contains("\"skills\""));
}

#[test]
fn human_explain_renders_every_safe_resolution_section_to_stderr() {
    let output = command()
        .args([
            "explain",
            &fixture("support-agent"),
            "--binding",
            &fixture("support-agent/bindings/deepagents.yaml"),
            "--target",
            &workspace_fixture("components/deepagents-test.json"),
            "--locked",
        ])
        .assert()
        .success();

    assert!(output.get_output().stdout.is_empty());
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    for expected in [
        "package: support-agent 0.1.0",
        "portable digest: 00165f500c6d060e774df30b153642d302ea478fe7b751e2a003a67ff3ac4977",
        "lock digest: 4a1f12410089007ca5ef1613faad7f5a5b3cf8921fef5cd0a71e75fa3982916e",
        "capabilities:",
        "cases.read@1.2.0 (required)",
        "models:",
        "primary -> models.anthropic.sonnet",
        "features:",
        "required: none",
        "enabled optional: none",
        "omitted optional: none",
        "precedence decisions:",
        "features: 0 required and 0 optional enabled",
        "models: 1 roles resolved",
        "child delegation boundaries: none",
        "diagnostics: none",
    ] {
        assert!(
            stderr.contains(expected),
            "missing human explain line: {expected}"
        );
    }
    assert!(!stderr.contains("Help support agents read cases safely."));
    assert!(!stderr.contains("prompts/system.md"));
}

#[test]
fn lock_is_the_command_that_writes_the_default_lock_path() {
    let (_directory, package) = copy_package("support-agent");
    fs::remove_file(package.join("capability.lock")).unwrap();

    let output = command()
        .args([
            "lock",
            package.to_str().unwrap(),
            "--catalog",
            &workspace_fixture("catalogs/support-v1.json"),
            "--json",
        ])
        .assert()
        .success();

    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["status"], "locked");
    assert!(package.join("capability.lock").is_file());
}

#[test]
fn non_mutating_commands_leave_the_package_unchanged() {
    let (_directory, package) = copy_package("support-agent");
    let lock_path = package.join("capability.lock");
    let before = fs::read(&lock_path).unwrap();

    for subcommand in ["check", "explain", "compile"] {
        let mut invocation = command();
        invocation.arg(subcommand).arg(&package);
        if subcommand != "check" {
            invocation
                .arg("--binding")
                .arg(package.join("bindings/deepagents.yaml"))
                .arg("--target")
                .arg(workspace_fixture("components/deepagents-test.json"));
        }
        invocation.args(["--locked", "--json"]).assert().success();
        assert_eq!(fs::read(&lock_path).unwrap(), before);
    }
}

#[test]
fn invalid_packages_use_the_stable_package_exit_category() {
    let output = command()
        .args(["check", &fixture("hostile/missing-file"), "--json"])
        .assert()
        .code(2);

    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["status"], "invalid");
    assert_eq!(body["diagnostics"][0]["category"], "package");
}

#[test]
fn json_mode_keeps_argument_errors_structured_and_prose_free() {
    let output = command()
        .args([
            "compile",
            &fixture("support-agent"),
            "--binding",
            &fixture("support-agent/bindings/deepagents.yaml"),
            "--target",
            &workspace_fixture("components/deepagents-test.json"),
            "--json",
        ])
        .assert()
        .code(2);

    assert!(output.get_output().stderr.is_empty());
    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["status"], "invalid");
    assert_eq!(body["diagnostics"][0]["code"], "KF-PKG-001");
}

#[test]
fn json_help_is_one_structured_result_without_usage_prose() {
    for args in [
        vec!["--json", "--help"],
        vec!["--help", "--json"],
        vec!["check", "package", "--json", "--help"],
    ] {
        let output = command().args(args).assert().code(2);
        assert!(output.get_output().stderr.is_empty());
        let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
        let body = parse_single_json(stdout.as_bytes());
        assert_eq!(body["status"], "invalid");
        assert!(!stdout.contains("Usage:"));
    }
}

#[test]
fn json_version_is_one_structured_result_without_version_prose() {
    for args in [
        vec!["--json", "--version"],
        vec!["--version", "--json"],
        vec!["check", "package", "--json", "--version"],
    ] {
        let output = command().args(args).assert().code(2);
        assert!(output.get_output().stderr.is_empty());
        let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
        let body = parse_single_json(stdout.as_bytes());
        assert_eq!(body["status"], "invalid");
        assert!(!stdout.contains("kiteframe 0.1.0"));
    }
}

#[test]
fn stale_locks_use_the_stable_lock_or_catalog_exit_category() {
    let (_directory, package) = copy_package("support-agent");
    fs::write(package.join("prompts/system.md"), "changed").unwrap();

    let output = command()
        .args(["check", package.to_str().unwrap(), "--locked", "--json"])
        .assert()
        .code(3);

    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["diagnostics"][0]["category"], "lock");
}

#[test]
fn unsupported_features_use_the_stable_resolution_exit_category() {
    let (_directory, package) = copy_package("support-agent");
    fs::write(
        package.join("agent.yaml"),
        r#"apiVersion: kiteframe.dev/v1alpha1
kind: Agent
metadata: { name: support-agent, version: 0.1.0 }
spec:
  prompt: { system: prompts/system.md }
  models:
    primary: { capabilities: [text, tool-calling] }
  capabilities:
    - { name: cases.read, version: "^1.0", required: true }
  features:
    required: [kiteframe.capability.point-of-use-auth@1]
"#,
    )
    .unwrap();
    command()
        .args([
            "lock",
            package.to_str().unwrap(),
            "--catalog",
            &workspace_fixture("catalogs/support-v1.json"),
            "--json",
        ])
        .assert()
        .success();

    let output = command()
        .args([
            "compile",
            package.to_str().unwrap(),
            "--binding",
            package.join("bindings/deepagents.yaml").to_str().unwrap(),
            "--target",
            &workspace_fixture("components/deepagents-test.json"),
            "--locked",
            "--json",
        ])
        .assert()
        .code(4);

    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["diagnostics"][0]["category"], "feature");
}

#[test]
fn runtime_target_failures_use_the_stable_runtime_exit_category() {
    let (_directory, package) = copy_package("support-agent");
    let target_path = package.join("wrong-target.json");
    fs::write(
        &target_path,
        r#"{"target":"other-runtime","components":{}}"#,
    )
    .unwrap();

    let output = command()
        .args([
            "compile",
            package.to_str().unwrap(),
            "--binding",
            package.join("bindings/deepagents.yaml").to_str().unwrap(),
            "--target",
            target_path.to_str().unwrap(),
            "--locked",
            "--json",
        ])
        .assert()
        .code(5);

    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["diagnostics"][0]["category"], "runtime");
}

#[test]
fn target_adapter_rejects_missing_explicit_target_metadata() {
    let (_directory, package) = copy_package("support-agent");
    let target_path = package.join("missing-target.json");
    fs::write(&target_path, r#"{"components":{}}"#).unwrap();

    let output = command()
        .args([
            "compile",
            package.to_str().unwrap(),
            "--binding",
            package.join("bindings/deepagents.yaml").to_str().unwrap(),
            "--target",
            target_path.to_str().unwrap(),
            "--locked",
            "--json",
        ])
        .assert()
        .code(5);

    let body = parse_single_json(&output.get_output().stdout);
    assert_eq!(body["diagnostics"][0]["code"], "KF-RUNTIME-001");
    assert_eq!(body["diagnostics"][0]["category"], "runtime");
}
