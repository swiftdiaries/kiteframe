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
    let decision = TypedAuthorizer::new(policy.clone())
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
    let compiled = compile(&policy, &catalog).unwrap();
    let cedar = CedarAuthorizer::new(&compiled, policy.clone()).unwrap();
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
    assert!(artifact.cedar.starts_with("permit("));
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
