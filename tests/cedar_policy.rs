use std::collections::{BTreeMap, BTreeSet};

use kiteframe::policy::compile;
use kiteframe::{
    Capability, CatalogTool, CedarAuthorizer, ClosedSchema, CreateSession, EffectKind,
    OneOfPrimitive, PolicyDecision, PolicyEffect, PolicyRequest, PolicyRule, PolicyTarget,
    PostgresStore, Primitive, RecoverySupport, SessionAuthorizer, SessionId, SessionPolicy,
    ToolCatalogSnapshot, TypedAuthorizer,
};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn typed_policy_validates_visible_targets_and_forbid_overrides_permit() {
    let catalog = ToolCatalogSnapshot::new(
        SessionId::new(Uuid::now_v7()),
        Uuid::now_v7(),
        [tool("docs/search")],
    )
    .unwrap();
    let policy = SessionPolicy {
        version: 1,
        rules: vec![
            PolicyRule {
                rule_id: "allow-search".to_owned(),
                effect: PolicyEffect::Permit,
                target: PolicyTarget::Tool {
                    names: BTreeSet::from(["docs/search".to_owned()]),
                },
                input: BTreeMap::new(),
            },
            PolicyRule {
                rule_id: "deny-unknown".to_owned(),
                effect: PolicyEffect::Forbid,
                target: PolicyTarget::Capability {
                    names: BTreeSet::from([Capability::McpUnknown]),
                },
                input: BTreeMap::new(),
            },
        ],
    };
    policy.validate(&catalog).unwrap();
    let decision = TypedAuthorizer::new(policy.clone(), &catalog)
        .unwrap()
        .authorize(&PolicyRequest {
            session: catalog.session_id().clone(),
            qualified_name: "docs/search".to_owned(),
            descriptor_digest: "digest".to_owned(),
            capabilities: vec![Capability::McpUnknown],
            arguments: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert_eq!(
        decision,
        PolicyDecision::Forbid {
            rule_ids: vec!["deny-unknown".to_owned()]
        }
    );
    let cedar = CedarAuthorizer::new(&policy, &catalog).unwrap();
    let cedar_decision = cedar
        .authorize(&PolicyRequest {
            session: catalog.session_id().clone(),
            qualified_name: "docs/search".to_owned(),
            descriptor_digest: catalog.tools()["docs/search"]
                .descriptor_digest()
                .to_owned(),
            capabilities: vec![Capability::McpUnknown],
            arguments: serde_json::json!({}),
        })
        .await
        .unwrap();
    assert_eq!(
        cedar_decision,
        PolicyDecision::Forbid {
            rule_ids: vec!["deny-unknown".to_owned()]
        }
    );
    let invisible = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "bad".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["admin/delete".to_owned()]),
            },
            input: BTreeMap::new(),
        }],
    };
    assert!(invisible.validate(&catalog).is_err());
}

#[test]
fn input_constraints_must_match_a_selected_tool_schema() {
    let mut properties = BTreeMap::new();
    properties.insert("query".to_owned(), ClosedSchema::string());
    let tool = CatalogTool::new(
        "docs/search",
        "docs",
        "test",
        ClosedSchema::object("input", properties),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .unwrap();
    let catalog =
        ToolCatalogSnapshot::new(SessionId::new(Uuid::now_v7()), Uuid::now_v7(), [tool]).unwrap();
    let constrained = |pointer: &str, values: Vec<Primitive>| SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "filter".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["docs/search".to_owned()]),
            },
            input: BTreeMap::from([(pointer.to_owned(), OneOfPrimitive(values))]),
        }],
    };
    assert!(
        constrained("/query", vec![Primitive::String("rust".to_owned())])
            .validate(&catalog)
            .is_ok()
    );
    assert!(
        constrained("/missing", vec![Primitive::String("rust".to_owned())])
            .validate(&catalog)
            .is_err()
    );
    assert!(
        constrained("/query", vec![Primitive::Integer(2)])
            .validate(&catalog)
            .is_err()
    );
}

#[test]
fn compiler_binds_exact_tool_rules_to_the_current_descriptor_digest() {
    let catalog = ToolCatalogSnapshot::new(
        SessionId::new(Uuid::now_v7()),
        Uuid::now_v7(),
        [tool("docs/search")],
    )
    .unwrap();
    let policy = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "allow-search".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["docs/search".to_owned()]),
            },
            input: BTreeMap::new(),
        }],
    };
    let artifact = compile(&policy, &catalog).unwrap();
    let expected = format!(
        "docs/search#{}",
        catalog.tools()["docs/search"].descriptor_digest()
    );
    assert!(artifact.cedar.contains(&expected));
    assert!(
        artifact
            .cedar
            .starts_with("@rule_id(\"allow-search\")\npermit(")
    );
    assert!(artifact.schema.contains("docs_search_"));
}

#[tokio::test]
async fn compiled_policy_updates_are_revision_checked_and_idempotent() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = PostgresStore::connect(&database_url).await.unwrap();
    let principal = kiteframe::PrincipalId::new(format!("policy-{}", Uuid::now_v7())).unwrap();
    let session = store
        .create_session(CreateSession {
            principal,
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .unwrap();
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [tool("docs/search")])
            .unwrap();
    store.append_catalog_epoch(&catalog).await.unwrap();
    let policy = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "allow-search".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["docs/search".to_owned()]),
            },
            input: BTreeMap::new(),
        }],
    };
    assert_eq!(
        store
            .append_session_policy(
                session.id.clone(),
                kiteframe::PolicyRevision::new(0),
                "policy-update".to_owned(),
                &policy,
                &catalog
            )
            .await
            .unwrap()
            .get(),
        1
    );
    assert_eq!(
        store
            .append_session_policy(
                session.id.clone(),
                kiteframe::PolicyRevision::new(0),
                "policy-update".to_owned(),
                &policy,
                &catalog
            )
            .await
            .unwrap()
            .get(),
        1
    );
    assert!(
        store
            .append_session_policy(
                session.id,
                kiteframe::PolicyRevision::new(0),
                "policy-update-2".to_owned(),
                &policy,
                &catalog
            )
            .await
            .is_err()
    );
}

fn tool(name: &str) -> CatalogTool {
    let alias = name.split_once('/').unwrap().0;
    CatalogTool::new(
        name,
        alias,
        "test",
        ClosedSchema::object("input", Default::default()),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .unwrap()
}

struct Case {
    label: &'static str,
    qualified_name: &'static str,
    stale_digest: bool,
    arguments: &'static str,
}

const CASES: &[Case] = &[
    Case {
        label: "push-missing-args",
        qualified_name: "repo/push",
        stale_digest: false,
        arguments: "{}",
    },
    Case {
        label: "push-api",
        qualified_name: "repo/push",
        stale_digest: false,
        arguments: r#"{"repo":"acme/api","force":false,"note":null,"opts":{"depth":1}}"#,
    },
    Case {
        label: "push-other-forced",
        qualified_name: "repo/push",
        stale_digest: false,
        arguments: r#"{"repo":"acme/other","force":true,"note":"hi","opts":{"depth":3}}"#,
    },
    Case {
        label: "push-quoted",
        qualified_name: "repo/push",
        stale_digest: false,
        arguments: r#"{"repo":"acme/\"api\"","force":false,"note":null}"#,
    },
    Case {
        label: "push-wrong-type",
        qualified_name: "repo/push",
        stale_digest: false,
        arguments: r#"{"repo":7,"force":"true","note":1.5,"opts":{"depth":18446744073709551615}}"#,
    },
    Case {
        label: "read",
        qualified_name: "docs/read",
        stale_digest: false,
        arguments: "{}",
    },
    Case {
        label: "read-stale-digest",
        qualified_name: "docs/read",
        stale_digest: true,
        arguments: "{}",
    },
    Case {
        label: "fetch",
        qualified_name: "web/fetch",
        stale_digest: false,
        arguments: "{}",
    },
    Case {
        label: "noop",
        qualified_name: "plain/noop",
        stale_digest: false,
        arguments: "{}",
    },
];

fn differential_catalog() -> ToolCatalogSnapshot {
    let with = |name: &str, input: BTreeMap<String, ClosedSchema>, caps: &[Capability]| {
        CatalogTool::new(
            name,
            name.split_once('/').unwrap().0,
            "test",
            ClosedSchema::object("input", input),
            ClosedSchema::object("output", Default::default()),
            caps.iter().cloned().collect(),
            EffectKind::ReadOnly,
            RecoverySupport::NotRequired,
        )
        .unwrap()
    };
    ToolCatalogSnapshot::new(
        SessionId::new(Uuid::now_v7()),
        Uuid::now_v7(),
        [
            with(
                "repo/push",
                BTreeMap::from([
                    ("repo".to_owned(), ClosedSchema::string()),
                    ("force".to_owned(), ClosedSchema::boolean()),
                    (
                        "note".to_owned(),
                        ClosedSchema::nullable(ClosedSchema::string()),
                    ),
                    (
                        "opts".to_owned(),
                        ClosedSchema::object(
                            "opts",
                            BTreeMap::from([("depth".to_owned(), ClosedSchema::integer())]),
                        ),
                    ),
                ]),
                &[Capability::WriteData, Capability::Network],
            ),
            with("docs/read", BTreeMap::new(), &[Capability::ReadData]),
            with(
                "web/fetch",
                BTreeMap::new(),
                &[Capability::ReadData, Capability::Network],
            ),
            with("plain/noop", BTreeMap::new(), &[]),
        ],
    )
    .unwrap()
}

fn differential_rules() -> Vec<(&'static str, Vec<PolicyRule>)> {
    let rule = |id: &str, effect: PolicyEffect, target: PolicyTarget| PolicyRule {
        rule_id: id.to_owned(),
        effect,
        target,
        input: BTreeMap::new(),
    };
    let tools = |names: &[&str]| PolicyTarget::Tool {
        names: names.iter().map(|name| (*name).to_owned()).collect(),
    };
    let caps = |names: &[Capability]| PolicyTarget::Capability {
        names: names.iter().cloned().collect(),
    };
    let limited = |id: &str, effect: PolicyEffect, input: &[(&str, Vec<Primitive>)]| PolicyRule {
        input: input
            .iter()
            .map(|(pointer, values)| ((*pointer).to_owned(), OneOfPrimitive(values.clone())))
            .collect(),
        ..rule(id, effect, tools(&["repo/push"]))
    };
    let string = |value: &str| Primitive::String(value.to_owned());
    vec![
        (
            "permit-tool-read",
            vec![rule("r", PolicyEffect::Permit, tools(&["docs/read"]))],
        ),
        (
            "forbid-tool-read",
            vec![rule("r", PolicyEffect::Forbid, tools(&["docs/read"]))],
        ),
        (
            "forbid-tool-push",
            vec![rule("r", PolicyEffect::Forbid, tools(&["repo/push"]))],
        ),
        (
            "permit-cap-read",
            vec![rule(
                "r",
                PolicyEffect::Permit,
                caps(&[Capability::ReadData]),
            )],
        ),
        (
            "permit-cap-read-network",
            vec![rule(
                "r",
                PolicyEffect::Permit,
                caps(&[Capability::ReadData, Capability::Network]),
            )],
        ),
        (
            "forbid-cap-network",
            vec![rule(
                "r",
                PolicyEffect::Forbid,
                caps(&[Capability::Network]),
            )],
        ),
        (
            "permit-push-api",
            vec![limited(
                "r",
                PolicyEffect::Permit,
                &[("/repo", vec![string("acme/api"), string("acme/\"api\"")])],
            )],
        ),
        (
            "permit-push-api-unforced",
            vec![limited(
                "r",
                PolicyEffect::Permit,
                &[
                    ("/repo", vec![string("acme/api")]),
                    ("/force", vec![Primitive::Boolean(false)]),
                ],
            )],
        ),
        (
            "permit-push-null-note",
            vec![limited(
                "r",
                PolicyEffect::Permit,
                &[("/note", vec![Primitive::Null])],
            )],
        ),
        (
            "permit-push-shallow",
            vec![limited(
                "r",
                PolicyEffect::Permit,
                &[(
                    "/opts/depth",
                    vec![Primitive::Integer(1), Primitive::Integer(2)],
                )],
            )],
        ),
        (
            "forbid-push-deep",
            vec![limited(
                "r",
                PolicyEffect::Forbid,
                &[("/opts/depth", vec![Primitive::Integer(3)])],
            )],
        ),
        (
            "forbid-push-null-note",
            vec![limited(
                "r",
                PolicyEffect::Forbid,
                &[("/note", vec![Primitive::Null])],
            )],
        ),
        (
            "forbid-push-forced",
            vec![limited(
                "r",
                PolicyEffect::Forbid,
                &[("/force", vec![Primitive::Boolean(true)])],
            )],
        ),
        (
            "permit-push+forbid-push-other",
            vec![
                rule("permit", PolicyEffect::Permit, tools(&["repo/push"])),
                limited(
                    "forbid",
                    PolicyEffect::Forbid,
                    &[("/repo", vec![string("acme/other")])],
                ),
            ],
        ),
        (
            "permit-tool-fetch+forbid-cap-network",
            vec![
                rule("permit", PolicyEffect::Permit, tools(&["web/fetch"])),
                rule("forbid", PolicyEffect::Forbid, caps(&[Capability::Network])),
            ],
        ),
    ]
}

fn case(label: &str) -> &'static Case {
    CASES.iter().find(|case| case.label == label).unwrap()
}

fn differential_request(catalog: &ToolCatalogSnapshot, case: &Case) -> PolicyRequest {
    let tool = &catalog.tools()[case.qualified_name];
    PolicyRequest {
        session: catalog.session_id().clone(),
        qualified_name: case.qualified_name.to_owned(),
        descriptor_digest: if case.stale_digest {
            "stale".to_owned()
        } else {
            tool.descriptor_digest().to_owned()
        },
        capabilities: tool.capabilities().iter().cloned().collect(),
        arguments: serde_json::from_str(case.arguments).unwrap(),
    }
}

#[tokio::test]
async fn typed_and_cedar_authorizers_agree() {
    let catalog = differential_catalog();
    let mut mismatches = Vec::new();
    for (label, rules) in differential_rules() {
        let policy = SessionPolicy { version: 1, rules };
        let typed = TypedAuthorizer::new(policy.clone(), &catalog).unwrap();
        let cedar = CedarAuthorizer::new(&policy, &catalog).unwrap();
        for case in CASES {
            let request = differential_request(&catalog, case);
            let typed_decision = typed.authorize(&request).await.unwrap();
            let cedar_decision = cedar.authorize(&request).await.unwrap();
            if typed_decision != cedar_decision {
                println!(
                    "{label} x {}: typed {typed_decision:?}, cedar {cedar_decision:?}",
                    case.label
                );
                mismatches.push((label, case.label));
            }
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:?}");
}

#[tokio::test]
async fn capability_permit_covers_only_tools_whose_capabilities_it_names() {
    let catalog = differential_catalog();
    let policy = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "read-only".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Capability {
                names: BTreeSet::from([Capability::ReadData]),
            },
            input: BTreeMap::new(),
        }],
    };
    let cedar = CedarAuthorizer::new(&policy, &catalog).unwrap();
    let decide = |name: &'static str| {
        let request = differential_request(
            &catalog,
            &Case {
                label: name,
                qualified_name: name,
                stale_digest: false,
                arguments: "{}",
            },
        );
        let cedar = &cedar;
        async move { cedar.authorize(&request).await.unwrap() }
    };
    assert_eq!(decide("docs/read").await, PolicyDecision::Allow);
    assert_eq!(decide("web/fetch").await, PolicyDecision::NeedsPermission);
    assert_eq!(decide("plain/noop").await, PolicyDecision::NeedsPermission);
}

#[tokio::test]
async fn tool_forbid_covers_a_tool_whose_contract_changed() {
    let catalog = differential_catalog();
    let (_, rules) = differential_rules()
        .into_iter()
        .find(|(label, _)| *label == "forbid-tool-read")
        .unwrap();
    let policy = SessionPolicy { version: 1, rules };
    let request = differential_request(&catalog, case("read-stale-digest"));
    assert_eq!(request.descriptor_digest, "stale");
    assert_eq!(
        CedarAuthorizer::new(&policy, &catalog)
            .unwrap()
            .authorize(&request)
            .await
            .unwrap(),
        PolicyDecision::Forbid {
            rule_ids: vec!["r".to_owned()]
        }
    );
}

#[tokio::test]
async fn capability_forbid_overrides_tool_permit_in_cedar() {
    let catalog = differential_catalog();
    let (_, rules) = differential_rules()
        .into_iter()
        .find(|(label, _)| *label == "permit-tool-fetch+forbid-cap-network")
        .unwrap();
    let policy = SessionPolicy { version: 1, rules };
    let compiled = compile(&policy, &catalog).unwrap();
    assert!(
        !compiled.cedar.contains("capability:"),
        "{}",
        compiled.cedar
    );
    let request = differential_request(&catalog, case("fetch"));
    assert_eq!(request.qualified_name, "web/fetch");
    assert_eq!(
        CedarAuthorizer::new(&policy, &catalog)
            .unwrap()
            .authorize(&request)
            .await
            .unwrap(),
        PolicyDecision::Forbid {
            rule_ids: vec!["forbid".to_owned()]
        }
    );
}

#[tokio::test]
async fn input_constraints_limit_which_arguments_a_rule_covers() {
    use PolicyDecision::{Allow, NeedsPermission};
    let catalog = differential_catalog();
    let forbid = |id: &str| PolicyDecision::Forbid {
        rule_ids: vec![id.to_owned()],
    };
    let expected = [
        ("permit-push-api", "push-api", Allow),
        ("permit-push-api", "push-quoted", Allow),
        ("permit-push-api", "push-other-forced", NeedsPermission),
        ("permit-push-api", "push-missing-args", NeedsPermission),
        ("permit-push-api", "push-wrong-type", NeedsPermission),
        ("permit-push-api-unforced", "push-api", Allow),
        ("permit-push-api-unforced", "push-quoted", NeedsPermission),
        ("permit-push-null-note", "push-api", Allow),
        ("permit-push-null-note", "push-missing-args", Allow),
        ("permit-push-null-note", "push-wrong-type", NeedsPermission),
        ("forbid-push-forced", "push-wrong-type", NeedsPermission),
        ("forbid-push-null-note", "push-wrong-type", forbid("r")),
        ("forbid-push-null-note", "push-missing-args", forbid("r")),
        ("forbid-push-null-note", "push-api", forbid("r")),
        (
            "forbid-push-null-note",
            "push-other-forced",
            NeedsPermission,
        ),
        (
            "permit-push-null-note",
            "push-other-forced",
            NeedsPermission,
        ),
        ("permit-push-shallow", "push-api", Allow),
        ("permit-push-shallow", "push-other-forced", NeedsPermission),
        ("permit-push-shallow", "push-missing-args", NeedsPermission),
        ("permit-push-shallow", "push-wrong-type", NeedsPermission),
        ("forbid-push-deep", "push-other-forced", forbid("r")),
        ("forbid-push-deep", "push-api", NeedsPermission),
        ("forbid-push-deep", "push-wrong-type", forbid("r")),
        ("forbid-push-forced", "push-other-forced", forbid("r")),
        ("forbid-push-forced", "push-missing-args", forbid("r")),
        ("forbid-push-forced", "push-api", NeedsPermission),
        ("permit-push+forbid-push-other", "push-api", Allow),
        (
            "permit-push+forbid-push-other",
            "push-other-forced",
            forbid("forbid"),
        ),
    ];
    let rules = differential_rules();
    for (rule_label, case_label, decision) in expected {
        let (_, rules) = rules
            .iter()
            .find(|(label, _)| *label == rule_label)
            .unwrap();
        let policy = SessionPolicy {
            version: 1,
            rules: rules.clone(),
        };
        let request = differential_request(&catalog, case(case_label));
        assert_eq!(
            CedarAuthorizer::new(&policy, &catalog)
                .unwrap()
                .authorize(&request)
                .await
                .unwrap(),
            decision,
            "{rule_label} x {case_label}"
        );
    }
}

#[tokio::test]
async fn input_constraint_strings_are_escaped_as_cedar_literals() {
    let catalog = differential_catalog();
    let awkward = "a\"b\\c\nd é\u{0}";
    let policy = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "awkward \"rule\"".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["repo/push".to_owned()]),
            },
            input: BTreeMap::from([(
                "/repo".to_owned(),
                OneOfPrimitive(vec![Primitive::String(awkward.to_owned())]),
            )]),
        }],
    };
    let cedar = CedarAuthorizer::new(&policy, &catalog).unwrap();
    let mut request = differential_request(&catalog, case("push-api"));
    request.arguments["repo"] = serde_json::json!(awkward);
    assert_eq!(
        cedar.authorize(&request).await.unwrap(),
        PolicyDecision::Allow
    );
    request.arguments["repo"] = serde_json::json!("a\"b");
    assert_eq!(
        cedar.authorize(&request).await.unwrap(),
        PolicyDecision::NeedsPermission
    );
}

#[test]
fn non_integer_number_constraints_are_rejected() {
    let mut properties = BTreeMap::new();
    properties.insert("ratio".to_owned(), ClosedSchema::number());
    let tool = CatalogTool::new(
        "math/scale",
        "math",
        "test",
        ClosedSchema::object("input", properties),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .unwrap();
    let catalog =
        ToolCatalogSnapshot::new(SessionId::new(Uuid::now_v7()), Uuid::now_v7(), [tool]).unwrap();
    let policy = SessionPolicy {
        version: 1,
        rules: vec![PolicyRule {
            rule_id: "ratio".to_owned(),
            effect: PolicyEffect::Permit,
            target: PolicyTarget::Tool {
                names: BTreeSet::from(["math/scale".to_owned()]),
            },
            input: BTreeMap::from([(
                "/ratio".to_owned(),
                OneOfPrimitive(vec![Primitive::Number(
                    serde_json::Number::from_f64(0.5).unwrap(),
                )]),
            )]),
        }],
    };
    assert!(matches!(
        policy.validate(&catalog),
        Err(kiteframe::PolicyError::UnsupportedNumber(pointer)) if pointer == "/ratio"
    ));
}
