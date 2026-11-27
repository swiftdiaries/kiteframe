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
}

const CASES: &[Case] = &[
    Case {
        label: "push",
        qualified_name: "repo/push",
        stale_digest: false,
    },
    Case {
        label: "read",
        qualified_name: "docs/read",
        stale_digest: false,
    },
    Case {
        label: "read-stale-digest",
        qualified_name: "docs/read",
        stale_digest: true,
    },
    Case {
        label: "fetch",
        qualified_name: "web/fetch",
        stale_digest: false,
    },
    Case {
        label: "noop",
        qualified_name: "plain/noop",
        stale_digest: false,
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
            "permit-tool-fetch+forbid-cap-network",
            vec![
                rule("permit", PolicyEffect::Permit, tools(&["web/fetch"])),
                rule("forbid", PolicyEffect::Forbid, caps(&[Capability::Network])),
            ],
        ),
    ]
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
    let request = differential_request(&catalog, &CASES[2]);
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
    let request = differential_request(&catalog, &CASES[3]);
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
