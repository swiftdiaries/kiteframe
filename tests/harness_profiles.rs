use std::collections::BTreeSet;
use std::fs;

use kiteframe::harness::{HarnessCapability, HarnessProfile, HarnessRouteTable};
use kiteframe::{HarnessConfig, HarnessRole, ModelRole};
use serde_json::Value;

fn profile(path: &str) -> HarnessProfile {
    HarnessProfile::from_json(&fs::read(path).expect("profile file")).expect("valid profile")
}

fn profile_id(value: &str) -> kiteframe::harness::HarnessProfileId {
    kiteframe::harness::HarnessProfileId::new(value).expect("profile id")
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
    let cases: [(&str, fn(&mut Value)); 4] = [
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
            r#"["pnpm","--dir","/opt/deepseek-harness","run","demo:acp"]"#,
        ),
        (
            "KITEFRAME_DSH_ACP_SOURCE_REVISION",
            "141eb6fef83422698aef7a981029e843e8161534",
        ),
        ("KITEFRAME_DSH_ACP_ENV_JSON", r#"{"PATH":"/opt/bin"}"#),
    ])
    .unwrap();

    let commands = config.external_harness_commands();
    assert_eq!(
        commands.codex.unwrap().program.to_string_lossy(),
        "/opt/codex-acp/bin/codex-acp"
    );
    assert_eq!(commands.deepseek.unwrap().program.to_string_lossy(), "pnpm");
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
        r#"["/usr/bin/env","bash","-c","echo"]"#,
        r#"["curl","--header","Authorization: Bearer value"]"#,
        r#"["curl","https://secret@example.test/acp"]"#,
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
}
