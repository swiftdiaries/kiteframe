use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiteframe::{
    BoundTool, Capability, CatalogTool, ClosedSchema, EffectKind, FrameId, IdempotencyKey,
    InProcessToolRegistry, PolicyRevision, RecoverySupport, RunId, Secret, SessionId, ToolCall,
    ToolCatalogSnapshot, ToolError, ToolResolver, ToolStatus, canonical_sha256,
};
use serde_json::json;
use uuid::Uuid;

struct TestBoundTool {
    descriptor: CatalogTool,
    invocations: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl BoundTool for TestBoundTool {
    fn descriptor(&self) -> &CatalogTool {
        &self.descriptor
    }

    async fn invoke(&self, _call: ToolCall) -> Result<serde_json::Value, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Ok(json!({}))
    }

    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        Ok(ToolStatus::Succeeded)
    }
}

fn test_bound_tool(descriptor: CatalogTool) -> (Arc<TestBoundTool>, Arc<AtomicUsize>) {
    let invocations = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(TestBoundTool {
            descriptor,
            invocations: Arc::clone(&invocations),
        }),
        invocations,
    )
}

fn block_on_ready<F: std::future::Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("test future unexpectedly yielded"),
    }
}

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
fn registry_resolves_only_tools_bound_to_the_requested_session() {
    let registry = InProcessToolRegistry::new();
    let tool = lookup_tool("docs/search");
    let digest = tool.descriptor_digest().to_owned();
    let (tool, _invocations) = test_bound_tool(tool);
    registry
        .register(tool)
        .expect("registry accepts a valid descriptor");

    let bound_session = SessionId::new(Uuid::nil());
    registry
        .snapshot(
            bound_session.clone(),
            Uuid::nil(),
            &BTreeSet::from(["docs/search".to_owned()]),
            &BTreeMap::from([("docs/search".to_owned(), digest.clone())]),
        )
        .expect("session snapshot is valid");

    assert!(
        registry
            .bound_tool(SessionId::new(Uuid::from_u128(1)), "docs/search", &digest)
            .is_err()
    );
    assert!(
        registry
            .bound_tool(bound_session, "docs/search", &digest)
            .is_ok()
    );
}

#[test]
fn resolver_rejects_a_call_validated_for_another_tool_before_invocation() {
    let registry = InProcessToolRegistry::new();
    let first = lookup_tool("docs/search");
    let second = lookup_tool("admin/search");
    let first_digest = first.descriptor_digest().to_owned();
    let second_digest = second.descriptor_digest().to_owned();
    let call = ToolCall::new(
        SessionId::new(Uuid::nil()),
        RunId::new(Uuid::nil()),
        FrameId::new(Uuid::nil()),
        PolicyRevision::new(1),
        &first,
        json!({"query": "first", "limit": null}),
        IdempotencyKey::new("call-1").expect("valid idempotency key"),
    )
    .expect("arguments are valid for the first tool");
    let (first, _first_invocations) = test_bound_tool(first);
    let (second, second_invocations) = test_bound_tool(second);
    registry.register(first).expect("first tool registers");
    registry.register(second).expect("second tool registers");

    let session = SessionId::new(Uuid::nil());
    registry
        .snapshot(
            session.clone(),
            Uuid::nil(),
            &BTreeSet::from(["docs/search".to_owned(), "admin/search".to_owned()]),
            &BTreeMap::from([
                ("docs/search".to_owned(), first_digest),
                ("admin/search".to_owned(), second_digest.clone()),
            ]),
        )
        .expect("session snapshot is valid");

    let result = block_on_ready(registry.invoke(session, "admin/search", &second_digest, call));

    assert!(result.is_err());
    assert_eq!(second_invocations.load(Ordering::SeqCst), 0);
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
