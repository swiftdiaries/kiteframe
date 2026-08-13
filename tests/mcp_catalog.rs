use std::sync::Arc;

use async_trait::async_trait;
use kiteframe::{
    AuthenticatedMcpPeer, ClosedSchema, FrameId, IdempotencyKey, McpError, McpLimits, McpManager,
    PolicyRevision, PrincipalId, RemoteTool, RunId, SessionId, ToolCall, ToolResolver,
    closed_schema,
};
use serde_json::json;
use tokio::sync::RwLock;
use uuid::Uuid;

#[tokio::test]
async fn discovery_is_principal_scoped_and_refreshes_only_its_own_catalog() {
    let manager = McpManager::new(McpLimits {
        max_servers: 2,
        max_pages_per_server: 2,
        max_tools: 8,
        max_descriptor_bytes: 1024,
        max_catalog_bytes: 4096,
    });
    let alice = Arc::new(FakePeer::new(vec![tool("search"), tool("shared")]));
    let bob = Arc::new(FakePeer::new(vec![tool("admin"), tool("shared")]));
    let alice_session = SessionId::new(Uuid::now_v7());
    let bob_session = SessionId::new(Uuid::now_v7());
    let alice_catalog = manager
        .attach_peer(
            alice_session.clone(),
            PrincipalId::new("alice").unwrap(),
            "docs".to_owned(),
            alice.clone(),
        )
        .await
        .unwrap();
    let alice_search_digest = alice_catalog.tools()["docs/search"]
        .descriptor_digest()
        .to_owned();
    let bob_catalog = manager
        .attach_peer(
            bob_session.clone(),
            PrincipalId::new("bob").unwrap(),
            "ops".to_owned(),
            bob,
        )
        .await
        .unwrap();
    assert!(alice_catalog.tools().contains_key("docs/search"));
    assert!(!alice_catalog.tools().contains_key("ops/admin"));
    assert!(bob_catalog.tools().contains_key("ops/admin"));
    assert!(!bob_catalog.tools().contains_key("docs/search"));

    alice.set(vec![tool("shared"), tool("new")]).await;
    let refresh = manager
        .refresh(alice_session.clone(), "docs")
        .await
        .unwrap();
    assert_eq!(refresh.added, ["docs/new"]);
    assert_eq!(refresh.removed, ["docs/search"]);
    assert!(refresh.changed.is_empty());
    assert!(matches!(
        ToolResolver::bound_tool(&manager, alice_session.clone(), "docs/search", &alice_search_digest),
        Err(kiteframe::ToolError::UnknownTool(name)) if name == "docs/search"
    ));
    assert!(
        ToolResolver::bound_tool(
            &manager,
            bob_session.clone(),
            "ops/admin",
            bob_catalog.tools()["ops/admin"].descriptor_digest(),
        )
        .is_ok()
    );
    let bound = manager
        .bound_tool(
            refresh.epoch.session_id().clone(),
            "docs/new",
            refresh.epoch.tools()["docs/new"].descriptor_digest(),
        )
        .await
        .unwrap();
    let call = ToolCall::new(
        refresh.epoch.session_id().clone(),
        RunId::new(Uuid::now_v7()),
        FrameId::new(Uuid::now_v7()),
        PolicyRevision::new(1),
        bound.descriptor(),
        json!({}),
        IdempotencyKey::new("call-1").unwrap(),
    )
    .unwrap();
    assert_eq!(bound.invoke(call).await.unwrap(), json!({}));
    assert!(matches!(
        manager
            .refresh(SessionId::new(Uuid::now_v7()), "docs")
            .await,
        Err(McpError::SessionNotAttached)
    ));
}

#[tokio::test]
async fn discovered_mcp_tools_cross_the_engine_resolver_boundary() {
    let manager = McpManager::new(McpLimits {
        max_servers: 1,
        max_pages_per_server: 1,
        max_tools: 4,
        max_descriptor_bytes: 1024,
        max_catalog_bytes: 4096,
    });
    let session = SessionId::new(Uuid::now_v7());
    let peer = Arc::new(FakePeer::new(vec![tool("search")]));
    let catalog = manager
        .attach_peer(
            session.clone(),
            PrincipalId::new("resolver-principal").unwrap(),
            "docs".to_owned(),
            peer,
        )
        .await
        .unwrap();
    let descriptor = catalog.tools().get("docs/search").unwrap();
    let bound = ToolResolver::bound_tool(
        &manager,
        session.clone(),
        "docs/search",
        descriptor.descriptor_digest(),
    )
    .expect("MCP catalog binding resolves through the engine boundary");
    let result = bound
        .invoke(
            ToolCall::new(
                session,
                RunId::new(Uuid::now_v7()),
                FrameId::new(Uuid::now_v7()),
                PolicyRevision::new(0),
                descriptor,
                json!({}),
                IdempotencyKey::new("mcp-resolver-call").unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result, json!({}));
}

#[test]
fn remote_schemas_are_closed_before_catalog_admission() {
    let valid = json!({"type":"object","additionalProperties":false,"required":["query"],"properties":{"query":{"type":"string"}}});
    assert!(closed_schema(&valid, "input").is_ok());
    for invalid in [
        json!({"type":"object","properties":{},"required":[]}),
        json!({"type":"object","additionalProperties":false,"properties":{"query":{"type":"string"}},"required":[]}),
        json!({"oneOf":[{"type":"string"},{"type":"number"}]}),
        json!({"enum":[null]}),
    ] {
        assert!(closed_schema(&invalid, "input").is_err());
    }
}

struct FakePeer {
    tools: RwLock<Vec<RemoteTool>>,
}
impl FakePeer {
    fn new(tools: Vec<RemoteTool>) -> Self {
        Self {
            tools: RwLock::new(tools),
        }
    }
    async fn set(&self, tools: Vec<RemoteTool>) {
        *self.tools.write().await = tools;
    }
}
#[async_trait]
impl AuthenticatedMcpPeer for FakePeer {
    async fn list_tools(
        &self,
        cursor: Option<String>,
    ) -> Result<(Vec<RemoteTool>, Option<String>), McpError> {
        if cursor.is_some() {
            return Ok((Vec::new(), None));
        }
        Ok((self.tools.read().await.clone(), None))
    }
    async fn call_tool(
        &self,
        _name: &str,
        _arguments: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        Ok(json!({}))
    }
}
fn tool(name: &str) -> RemoteTool {
    RemoteTool {
        name: name.to_owned(),
        description: "visible only to this principal".to_owned(),
        input: ClosedSchema::object("input", Default::default()),
        output: ClosedSchema::object("output", Default::default()),
        metadata: json!({"server_metadata_must_not_grant_capabilities": true}),
    }
}
