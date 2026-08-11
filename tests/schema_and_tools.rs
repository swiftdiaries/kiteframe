use std::collections::{BTreeMap, BTreeSet};

use kiteframe::{
    Capability, CatalogTool, ClosedSchema, EffectKind, FrameId, IdempotencyKey,
    InProcessToolRegistry, PolicyRevision, RecoverySupport, RunId, Secret, SessionId, ToolCall,
    ToolCatalogSnapshot, canonical_sha256,
};
use serde_json::json;
use uuid::Uuid;

fn object_schema() -> ClosedSchema {
    ClosedSchema::object(
        "lookup_input",
        BTreeMap::from([
            ("query".into(), ClosedSchema::string()),
            (
                "limit".into(),
                ClosedSchema::nullable(ClosedSchema::integer()),
            ),
        ]),
    )
}

fn lookup_tool(name: &str) -> CatalogTool {
    let server_alias = name
        .split_once('/')
        .expect("test fixture has a qualified name")
        .0;
    CatalogTool::new(
        name,
        server_alias,
        "Returns the matching records.",
        object_schema(),
        ClosedSchema::object("lookup_output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("a read-only descriptor is valid")
}

#[test]
fn required_nullable_fields_accept_null_but_not_missing() {
    let schema = object_schema();
    assert!(
        schema
            .validate(&json!({"query": "first", "limit": null}))
            .is_ok()
    );
    assert!(schema.validate(&json!({"query": "first"})).is_err());
}

#[test]
fn closed_objects_reject_unknown_properties() {
    let schema = object_schema();
    let error = schema
        .validate(&json!({"query": "first", "limit": 3, "unexpected": true}))
        .expect_err("closed objects reject unknown fields");
    assert!(error.to_string().contains("unexpected"));
}

#[test]
fn descriptor_digest_is_canonical_across_property_insertion_order() {
    let first = lookup_tool("docs/search");
    let reordered_input = ClosedSchema::object(
        "lookup_input",
        BTreeMap::from([
            (
                "limit".into(),
                ClosedSchema::nullable(ClosedSchema::integer()),
            ),
            ("query".into(), ClosedSchema::string()),
        ]),
    );
    let second = CatalogTool::new(
        "docs/search",
        "docs",
        "Returns the matching records.",
        reordered_input,
        ClosedSchema::object("lookup_output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("descriptor is valid");

    assert_eq!(first.descriptor_digest(), second.descriptor_digest());
    assert_eq!(
        canonical_sha256(&json!({"b": 2, "a": 1})).expect("canonical JSON"),
        "43258cff783fe7036d8a43033f830adfc60ec037382473548ac742b888292777"
    );
}

#[test]
fn catalog_rejects_qualified_name_collisions() {
    let session = SessionId::new(Uuid::nil());
    let tools = vec![lookup_tool("docs/search"), lookup_tool("docs/search")];
    assert!(ToolCatalogSnapshot::new(session, Uuid::nil(), tools).is_err());
}

#[test]
fn effectful_tools_require_recovery_support() {
    let result = CatalogTool::new(
        "billing/charge",
        "billing",
        "Charges a payment method.",
        object_schema(),
        ClosedSchema::object("charge_output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::Effectful,
        RecoverySupport::Unsupported,
    );
    assert!(result.is_err());
}

#[test]
fn effectful_tools_reject_not_required_recovery() {
    let result = CatalogTool::new(
        "billing/charge",
        "billing",
        "Charges a payment method.",
        object_schema(),
        ClosedSchema::object("charge_output", BTreeMap::new()),
        BTreeSet::new(),
        EffectKind::Effectful,
        RecoverySupport::NotRequired,
    );
    assert!(result.is_err());
}

#[test]
fn unknown_mcp_tools_default_to_mcp_unknown() {
    let registry = InProcessToolRegistry::new();
    let tool = registry
        .unknown_mcp_tool(
            "files",
            "read",
            "Reads a file",
            object_schema(),
            object_schema(),
        )
        .expect("valid unknown MCP descriptor");
    assert_eq!(tool.qualified_name(), "files/read");
    assert_eq!(
        tool.capabilities(),
        &BTreeSet::from([Capability::McpUnknown])
    );
}

#[test]
fn model_visible_tools_are_the_intersection_of_visibility_and_pinned_bindings() {
    let session = SessionId::new(Uuid::nil());
    let visible = lookup_tool("docs/search");
    let hidden = lookup_tool("admin/search");
    let catalog = ToolCatalogSnapshot::new(session, Uuid::nil(), vec![visible.clone(), hidden])
        .expect("catalog is valid");

    let tools = catalog
        .model_visible_tools(
            &BTreeSet::from(["docs/search".to_owned()]),
            &BTreeMap::from([(
                "docs/search".to_owned(),
                visible.descriptor_digest().to_owned(),
            )]),
        )
        .expect("visible tool has the immutable binding's exact digest");

    assert_eq!(tools.into_keys().collect::<Vec<_>>(), vec!["docs/search"]);
}

#[test]
fn model_visible_tools_reject_a_changed_bound_descriptor_digest() {
    let session = SessionId::new(Uuid::nil());
    let tool = lookup_tool("docs/search");
    let catalog =
        ToolCatalogSnapshot::new(session, Uuid::nil(), vec![tool]).expect("catalog is valid");

    let result = catalog.model_visible_tools(
        &BTreeSet::from(["docs/search".to_owned()]),
        &BTreeMap::from([("docs/search".to_owned(), "wrong-digest".to_owned())]),
    );

    assert!(result.is_err());
}

#[test]
fn untrusted_capability_tags_are_rejected() {
    assert!(Capability::from_deployment_tag("model-claimed-admin").is_err());
}

#[test]
fn secret_debug_is_redacted() {
    let secret = Secret::new("do-not-log-this");
    assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
}

#[test]
fn tool_calls_reject_arguments_that_do_not_match_the_input_schema() {
    let tool = lookup_tool("docs/search");
    let result = ToolCall::new(
        SessionId::new(Uuid::nil()),
        RunId::new(Uuid::nil()),
        FrameId::new(Uuid::nil()),
        PolicyRevision::new(1),
        &tool,
        json!({"query": "first"}),
        IdempotencyKey::new("call-1").expect("valid idempotency key"),
    );

    assert!(result.is_err());
}
