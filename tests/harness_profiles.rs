use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use kiteframe::harness::{HarnessCapability, HarnessProfile, HarnessRouteTable};
use kiteframe::{ConfigError, HarnessConfig, HarnessRole, ModelRole};
use serde_json::Value;
use uuid::Uuid;

type MalformedProfileCase = (&'static str, fn(&mut Value));

fn profile(path: &str) -> HarnessProfile {
    HarnessProfile::from_json(&fs::read(path).expect("profile file")).expect("valid profile")
}

fn profile_id(value: &str) -> kiteframe::harness::HarnessProfileId {
    kiteframe::harness::HarnessProfileId::new(value).expect("profile id")
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("kiteframe-harness-profiles-{}", Uuid::now_v7()));
        fs::create_dir_all(&path).expect("temporary test directory");
        Self(path)
    }

    fn join(&self, value: &str) -> PathBuf {
        self.0.join(value)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn v1_profiles_pin_exact_packages_and_share_only_the_portable_core() {
    let codex = profile("config/harness-profiles/codex-acp-v1.json");
    let deepseek = profile("config/harness-profiles/deepseek-harness-acp-v1.json");
    let core = BTreeSet::from([
        HarnessCapability::FreshSession,
        HarnessCapability::TextPrompt,
        HarnessCapability::PermissionOnce,
        HarnessCapability::Cancel,
        HarnessCapability::CommittedOutput,
    ]);
    assert_eq!(
        codex.source_revision,
        "296069e841634cd4bb9bc4515602d836e49231ec"
    );
    assert_eq!(
        deepseek.source_revision,
        "141eb6fef83422698aef7a981029e843e8161534"
    );
    assert_eq!(codex.required, core);
    assert_eq!(deepseek.required, core);
    assert!(deepseek.fresh_sessions_only);
    assert!(!deepseek.optional.contains(&HarnessCapability::SessionLoad));
}

#[test]
fn absent_optional_capability_fails_closed_when_requested() {
    let deepseek = profile("config/harness-profiles/deepseek-harness-acp-v1.json");
    let error = deepseek
        .require(HarnessCapability::SessionLoad)
        .unwrap_err();
    assert_eq!(error.code, "harness_capability_unsupported");
}

#[test]
fn deployment_route_table_selects_profiles_without_changing_kite_source() {
    let routes = HarnessRouteTable::new(profile_id("builtin-v1"), profile_id("codex-acp-v1"));
    assert_eq!(
        routes
            .select(Some(HarnessRole::Coding), ModelRole::Reasoning)
            .as_str(),
        "codex-acp-v1"
    );
    assert_eq!(
        routes.select(None, ModelRole::Reasoning).as_str(),
        "builtin-v1"
    );
}

#[test]
fn optional_capability_absent_from_handshake_fails_closed() {
    let codex = profile("config/harness-profiles/codex-acp-v1.json");
    let observed = kiteframe::harness::HarnessCapabilities {
        protocol_version: 1,
        implementation_name: "codex-acp".into(),
        implementation_version: "test".into(),
        supported: codex.required.clone(),
    };
    codex
        .validate_observed(&observed)
        .expect("portable core present");
    let error = codex
        .require_observed(HarnessCapability::SessionLoad, &observed)
        .unwrap_err();
    assert_eq!(error.code, "harness_capability_unsupported");
}

#[test]
fn optional_capability_requires_observed_handshake_state() {
    let codex = profile("config/harness-profiles/codex-acp-v1.json");
    assert_eq!(
        codex
            .require(HarnessCapability::SessionLoad)
            .unwrap_err()
            .code,
        "harness_capability_unsupported"
    );
    let mut supported = codex.required.clone();
    supported.insert(HarnessCapability::SessionLoad);
    let observed = kiteframe::harness::HarnessCapabilities {
        protocol_version: 1,
        implementation_name: "codex-acp".into(),
        implementation_version: "test".into(),
        supported,
    };
    codex
        .require_observed(HarnessCapability::SessionLoad, &observed)
        .expect("observed optional capability is supported");
}

#[test]
fn malformed_profiles_fail_closed_with_stable_error_code() {
    let source =
        fs::read_to_string("config/harness-profiles/deepseek-harness-acp-v1.json").unwrap();
    let cases: [MalformedProfileCase; 4] = [
        ("unknown field", |value: &mut Value| {
            value["unexpected"] = Value::Bool(true);
        }),
        ("duplicate capability", |value: &mut Value| {
            value["required"]
                .as_array_mut()
                .unwrap()
                .push(Value::String("cancel".into()));
        }),
        ("protocol is not profile JSON", |value: &mut Value| {
            value["sourceRevision"] = Value::String("not-a-full-revision".into());
        }),
        ("unexpected revision", |value: &mut Value| {
            value["sourceRevision"] =
                Value::String("0000000000000000000000000000000000000000".into());
        }),
    ];
    for (name, mutate) in cases {
        let mut value: Value = serde_json::from_str(&source).unwrap();
        mutate(&mut value);
        let error = HarnessProfile::from_json(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert_eq!(error.code, "harness_profile_unavailable", "{name}");
    }
}

#[test]
fn observed_protocol_mismatch_fails_closed() {
    let deepseek = profile("config/harness-profiles/deepseek-harness-acp-v1.json");
    let error = deepseek
        .validate_observed(&kiteframe::harness::HarnessCapabilities {
            protocol_version: 2,
            implementation_name: "deepseek".into(),
            implementation_version: "test".into(),
            supported: deepseek.required.clone(),
        })
        .unwrap_err();
    assert_eq!(error.code, "harness_protocol_incompatible");
}

#[test]
fn command_overrides_are_json_arrays_without_shell_interpolation() {
    let config = HarnessConfig::from_test_values([
        (
            "KITEFRAME_CODEX_ACP_COMMAND_JSON",
            r#"["/opt/codex-acp/bin/codex-acp"]"#,
        ),
        (
            "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
            "296069e841634cd4bb9bc4515602d836e49231ec",
        ),
        (
            "KITEFRAME_DSH_ACP_COMMAND_JSON",
            r#"["echo","--dir","/opt/deepseek-harness","run","demo:acp"]"#,
        ),
        (
            "KITEFRAME_DSH_ACP_SOURCE_REVISION",
            "141eb6fef83422698aef7a981029e843e8161534",
        ),
        ("KITEFRAME_DSH_ACP_ENV_JSON", r#"{"PATH":"/bin"}"#),
    ])
    .unwrap();

    let commands = config.external_harness_commands();
    assert_eq!(
        commands.codex.unwrap().program.to_string_lossy(),
        "/opt/codex-acp/bin/codex-acp"
    );
    assert!(commands.deepseek.unwrap().program.is_absolute());
}

#[test]
fn resolved_executable_identity_is_absolute_and_immune_to_workspace_shadowing() {
    let temporary = TemporaryDirectory::new();
    let trusted = temporary.join("trusted");
    let workspace = temporary.join("workspace");
    fs::create_dir_all(&trusted).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let trusted_program = trusted.join("pnpm");
    fs::write(&trusted_program, "trusted").unwrap();
    fs::write(workspace.join("pnpm"), "workspace shadow").unwrap();

    let path = trusted.to_string_lossy().into_owned();
    let environment = format!(r#"{{"PATH":"{path}"}}"#);
    let config = HarnessConfig::from_test_values([
        ("KITEFRAME_DSH_ACP_COMMAND_JSON", r#"["pnpm","run","demo"]"#),
        (
            "KITEFRAME_DSH_ACP_SOURCE_REVISION",
            "141eb6fef83422698aef7a981029e843e8161534",
        ),
        ("KITEFRAME_DSH_ACP_ENV_JSON", environment.as_str()),
    ])
    .unwrap();
    assert_eq!(
        config.external_harness_commands().deepseek.unwrap().program,
        fs::canonicalize(&trusted_program).unwrap()
    );

    for invalid_path in [
        format!("relative:{path}"),
        format!(":{path}"),
        format!("{path}:"),
    ] {
        let invalid_environment = format!(r#"{{"PATH":"{invalid_path}"}}"#);
        assert!(
            HarnessConfig::from_test_values([
                ("KITEFRAME_DSH_ACP_COMMAND_JSON", r#"["pnpm"]"#),
                (
                    "KITEFRAME_DSH_ACP_SOURCE_REVISION",
                    "141eb6fef83422698aef7a981029e843e8161534",
                ),
                ("KITEFRAME_DSH_ACP_ENV_JSON", invalid_environment.as_str()),
            ])
            .is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn resolved_absolute_symlink_identity_is_checked_by_target_basename() {
    use std::os::unix::fs::symlink;

    let temporary = TemporaryDirectory::new();
    let target = temporary.join("ash");
    let link = temporary.join("configured-acp");
    fs::write(&target, "not a real interpreter").unwrap();
    symlink(&target, &link).unwrap();
    let command = serde_json::json!([link.to_string_lossy()]).to_string();
    assert!(
        HarnessConfig::from_test_values([
            ("KITEFRAME_CODEX_ACP_COMMAND_JSON", command.as_str()),
            (
                "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                "296069e841634cd4bb9bc4515602d836e49231ec",
            ),
        ])
        .is_err()
    );
}

#[test]
fn external_commands_require_complete_attestation_and_safe_literal_arguments() {
    let cases = [
        (
            "command without a source revision",
            [("KITEFRAME_CODEX_ACP_COMMAND_JSON", r#"["codex-acp"]"#)].as_slice(),
        ),
        (
            "source revision without a command",
            [(
                "KITEFRAME_DSH_ACP_SOURCE_REVISION",
                "141eb6fef83422698aef7a981029e843e8161534",
            )]
            .as_slice(),
        ),
        (
            "shell command form",
            [
                (
                    "KITEFRAME_CODEX_ACP_COMMAND_JSON",
                    r#"["sh","-c","codex-acp"]"#,
                ),
                (
                    "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                    "296069e841634cd4bb9bc4515602d836e49231ec",
                ),
            ]
            .as_slice(),
        ),
        (
            "secret command argument",
            [
                (
                    "KITEFRAME_DSH_ACP_COMMAND_JSON",
                    r#"["pnpm","--token=secret-value"]"#,
                ),
                (
                    "KITEFRAME_DSH_ACP_SOURCE_REVISION",
                    "141eb6fef83422698aef7a981029e843e8161534",
                ),
            ]
            .as_slice(),
        ),
    ];

    for (name, values) in cases {
        assert!(
            HarnessConfig::from_test_values(values.iter().copied()).is_err(),
            "{name}"
        );
    }
}

#[test]
fn harness_routes_default_to_builtin_and_preserve_selected_unconfigured_profile() {
    let current_builtin = profile_id("builtin-v1-0123456789abcdef0123456789abcdef01234567");
    let routes = HarnessConfig::from_test_values([
        ("KITEFRAME_REASONING_HARNESS_PROFILE", "codex-acp-v1"),
        (
            "KITEFRAME_CODING_HARNESS_PROFILE",
            "deepseek-harness-acp-v1",
        ),
    ])
    .unwrap()
    .harness_routes(current_builtin.clone());
    assert_eq!(routes.reasoning.as_str(), "codex-acp-v1");
    assert_eq!(routes.coding.as_str(), "deepseek-harness-acp-v1");

    let defaults = HarnessConfig::from_test_values(std::iter::empty::<(&str, &str)>())
        .unwrap()
        .harness_routes(current_builtin.clone());
    assert_eq!(defaults.reasoning, current_builtin);
    assert_eq!(defaults.coding, defaults.reasoning);
}

#[test]
fn external_environment_is_explicit_redacted_and_requires_path_for_bare_commands() {
    let config = HarnessConfig::from_test_values([
        ("KITEFRAME_CODEX_ACP_COMMAND_JSON", r#"["/opt/codex-acp"]"#),
        (
            "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
            "296069e841634cd4bb9bc4515602d836e49231ec",
        ),
        (
            "KITEFRAME_CODEX_ACP_ENV_JSON",
            r#"{"CODEX_TOKEN":"do-not-log","HOME":"/sealed/home"}"#,
        ),
        (
            "KITEFRAME_CODEX_ACP_REQUIRED_ENV_JSON",
            r#"["CODEX_TOKEN"]"#,
        ),
    ])
    .unwrap();
    let environment = config.external_harness_environments().codex.unwrap();
    assert!(environment.contains_key("CODEX_TOKEN"));
    assert!(environment.required_values_present());
    assert!(!format!("{environment:?}").contains("do-not-log"));

    let bare_without_path = HarnessConfig::from_test_values([
        (
            "KITEFRAME_DSH_ACP_COMMAND_JSON",
            r#"["pnpm","run","demo:acp"]"#,
        ),
        (
            "KITEFRAME_DSH_ACP_SOURCE_REVISION",
            "141eb6fef83422698aef7a981029e843e8161534",
        ),
    ]);
    assert!(bare_without_path.is_err());
}

#[test]
fn literal_arguments_allow_metacharacters_but_reject_shell_and_precise_secret_forms() {
    let safe = HarnessConfig::from_test_values([
        (
            "KITEFRAME_CODEX_ACP_COMMAND_JSON",
            r#"["/opt/codex-acp","--label=a&b;$(literal)"]"#,
        ),
        (
            "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
            "296069e841634cd4bb9bc4515602d836e49231ec",
        ),
    ]);
    assert!(safe.is_ok());

    for command in [
        r#"["/bin/sh","-c","echo"]"#,
        r#"["/bin/dash","-c","echo"]"#,
        r#"["/bin/ksh","-c","echo"]"#,
        r#"["/bin/csh","-c","echo"]"#,
        r#"["/bin/tcsh","-c","echo"]"#,
        r#"["/bin/fish","-c","echo"]"#,
        r#"["/bin/ash","-c","echo"]"#,
        r#"["/bin/hush","-c","echo"]"#,
        r#"["/bin/busybox","sh","-c","echo"]"#,
        r#"["/bin/busybox","ash","-c","echo"]"#,
        r#"["/bin/busybox","hush","-c","echo"]"#,
        r#"["/bin/busybox","--install","/tmp/bin"]"#,
        r#"["/usr/bin/env","bash","-c","echo"]"#,
        r#"["/usr/bin/env","printenv"]"#,
        r#"["/usr/bin/command","dash","-c","echo"]"#,
        r#"["curl","--header","Authorization: Bearer value"]"#,
        r#"["curl","--header=Authorization: Bearer value"]"#,
        r#"["curl","-HAuthorization: Bearer value"]"#,
        r#"["curl","--header","Proxy-Authorization: Bearer value"]"#,
        r#"["curl","--header=Proxy-Authorization: Bearer value"]"#,
        r#"["curl","-HProxy-Authorization: Bearer value"]"#,
        r#"["curl","--user=person:password"]"#,
        r#"["curl","-uperson:password"]"#,
        r#"["curl","--token:value"]"#,
        r#"["curl","https://secret@example.test/acp"]"#,
        r#"["curl","https://example.test/acp?api_key=secret"]"#,
        r#"["curl","https://example.test/acp?%74oken=secret"]"#,
        r#"["curl","https://example.test/acp?%6bey=secret"]"#,
        r#"["curl","https://example.test/acp?%70assword=secret"]"#,
        r#"["curl","https://example.test/acp?%61uth=secret"]"#,
        r#"["curl","https://example.test/acp?%ZZ=ambiguous"]"#,
    ] {
        assert!(
            HarnessConfig::from_test_values([
                ("KITEFRAME_CODEX_ACP_COMMAND_JSON", command),
                (
                    "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                    "296069e841634cd4bb9bc4515602d836e49231ec",
                ),
            ])
            .is_err()
        );
    }

    let benign = HarnessConfig::from_test_values([
        (
            "KITEFRAME_CODEX_ACP_COMMAND_JSON",
            r#"["/opt/codex-acp","--tokenizer=enabled","https://example.test/acp?topic=ok"]"#,
        ),
        (
            "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
            "296069e841634cd4bb9bc4515602d836e49231ec",
        ),
    ]);
    assert!(benign.is_ok());

    let email_query = HarnessConfig::from_test_values([
        (
            "KITEFRAME_CODEX_ACP_COMMAND_JSON",
            r#"["/opt/codex-acp","https://example.test?email=user@example.com"]"#,
        ),
        (
            "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
            "296069e841634cd4bb9bc4515602d836e49231ec",
        ),
    ]);
    assert!(email_query.is_ok());
}

#[test]
fn required_environment_values_and_json_keys_are_unique_and_nonempty() {
    for environment in [
        r#"{"CODEX_TOKEN":""}"#,
        r#"{"CODEX_TOKEN":"   "}"#,
        r#"{"CODEX_TOKEN":"one","CODEX_TOKEN":"two"}"#,
    ] {
        assert!(
            HarnessConfig::from_test_values([
                ("KITEFRAME_CODEX_ACP_COMMAND_JSON", r#"["/opt/codex-acp"]"#),
                (
                    "KITEFRAME_CODEX_ACP_SOURCE_REVISION",
                    "296069e841634cd4bb9bc4515602d836e49231ec",
                ),
                ("KITEFRAME_CODEX_ACP_ENV_JSON", environment),
                (
                    "KITEFRAME_CODEX_ACP_REQUIRED_ENV_JSON",
                    r#"["CODEX_TOKEN"]"#
                ),
            ])
            .is_err()
        );
    }
    for path in ["", "   "] {
        let empty_path = HarnessConfig::from_test_values([
            ("KITEFRAME_DSH_ACP_COMMAND_JSON", r#"["pnpm","run","demo"]"#),
            (
                "KITEFRAME_DSH_ACP_SOURCE_REVISION",
                "141eb6fef83422698aef7a981029e843e8161534",
            ),
            (
                "KITEFRAME_DSH_ACP_ENV_JSON",
                if path.is_empty() {
                    r#"{"PATH":""}"#
                } else {
                    r#"{"PATH":"   "}"#
                },
            ),
        ])
        .unwrap_err();
        assert!(matches!(
            empty_path,
            ConfigError::BareExternalHarnessCommandRequiresPath { .. }
        ));
    }
    assert!(
        HarnessConfig::from_test_values([
            ("KITEFRAME_DSH_ACP_COMMAND_JSON", r#"["/opt/dsh"]"#),
            (
                "KITEFRAME_DSH_ACP_SOURCE_REVISION",
                "141eb6fef83422698aef7a981029e843e8161534",
            ),
            ("KITEFRAME_DSH_ACP_ENV_JSON", r#"{"AUTH":"ok"}"#),
            ("KITEFRAME_DSH_ACP_REQUIRED_ENV_JSON", r#"["AUTH","AUTH"]"#),
        ])
        .is_err()
    );
    for environment in [
        r#"{"AUTH":"ok"} trailing"#,
        r#"{"AUTH":"ok"}{"LEAK":"value"}"#,
    ] {
        assert!(
            HarnessConfig::from_test_values([
                ("KITEFRAME_DSH_ACP_COMMAND_JSON", r#"["/opt/dsh"]"#),
                (
                    "KITEFRAME_DSH_ACP_SOURCE_REVISION",
                    "141eb6fef83422698aef7a981029e843e8161534",
                ),
                ("KITEFRAME_DSH_ACP_ENV_JSON", environment),
                ("KITEFRAME_DSH_ACP_REQUIRED_ENV_JSON", r#"["AUTH"]"#),
            ])
            .is_err()
        );
    }
}
