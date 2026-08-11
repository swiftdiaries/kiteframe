use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    CatalogEpochId, FrameId, HashError, IdempotencyKey, PolicyRevision, RunId, SchemaError,
    SessionId,
};

use super::CatalogTool;

mod private {
    pub trait Sealed {}
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub frame_id: FrameId,
    pub policy_revision: PolicyRevision,
    arguments: ValidatedArguments,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug)]
pub struct ValidatedArguments {
    target_qualified_name: String,
    target_descriptor_digest: String,
    value: Value,
}

impl ValidatedArguments {
    pub fn for_tool(tool: &CatalogTool, arguments: Value) -> Result<Self, ToolError> {
        tool.verify_descriptor_digest()?;
        tool.input()
            .validate(&arguments)
            .map_err(ToolError::InvalidArguments)?;
        Ok(Self {
            target_qualified_name: tool.qualified_name().to_owned(),
            target_descriptor_digest: tool.descriptor_digest().to_owned(),
            value: arguments,
        })
    }

    #[must_use]
    pub const fn as_json(&self) -> &Value {
        &self.value
    }

    fn verify_target(&self, tool: &CatalogTool) -> Result<(), ToolError> {
        tool.verify_descriptor_digest()?;
        if self.target_qualified_name == tool.qualified_name()
            && self.target_descriptor_digest == tool.descriptor_digest()
        {
            Ok(())
        } else {
            Err(ToolError::ToolContractChanged {
                qualified_name: tool.qualified_name().to_owned(),
            })
        }
    }
}

impl ToolCall {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        run_id: RunId,
        frame_id: FrameId,
        policy_revision: PolicyRevision,
        tool: &CatalogTool,
        arguments: Value,
        idempotency_key: IdempotencyKey,
    ) -> Result<Self, ToolError> {
        Ok(Self {
            session_id,
            run_id,
            frame_id,
            policy_revision,
            arguments: ValidatedArguments::for_tool(tool, arguments)?,
            idempotency_key,
        })
    }

    #[must_use]
    pub const fn arguments(&self) -> &ValidatedArguments {
        &self.arguments
    }

    pub(crate) fn verify_target(&self, tool: &CatalogTool) -> Result<(), ToolError> {
        self.arguments.verify_target(tool)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    WaitingForPermission,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolFailure {
    ToolContractChanged,
    WaitingForPermission,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid qualified tool name: {0}")]
    InvalidQualifiedName(String),
    #[error("duplicate qualified tool name: {0}")]
    DuplicateQualifiedName(String),
    #[error("unsupported closed schema: {0}")]
    UnsupportedSchema(#[source] SchemaError),
    #[error("unsafe recovery support for effectful tool: {0}")]
    UnsafeEffectRecovery(String),
    #[error("tool contract changed: {qualified_name}")]
    ToolContractChanged { qualified_name: String },
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("unknown session")]
    UnknownSession(SessionId),
    #[error("catalog epoch mismatch")]
    CatalogEpochMismatch(CatalogEpochId),
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(#[source] SchemaError),
    #[error("canonical descriptor error: {0}")]
    Canonicalization(#[source] HashError),
    #[error("tool invocation failed")]
    InvocationFailed,
}

#[async_trait]
#[allow(private_bounds)]
pub trait BoundTool: private::Sealed + Send + Sync {
    fn descriptor(&self) -> &CatalogTool;
    async fn invoke_inner(&self, call: ToolCall) -> Result<Value, ToolError>;
    async fn invoke(&self, call: ToolCall) -> Result<Value, ToolError> {
        call.verify_target(self.descriptor())?;
        self.invoke_inner(call).await
    }
    async fn status(&self, key: &IdempotencyKey) -> Result<ToolStatus, ToolError>;
}

#[async_trait]
pub trait ToolResolver: Send + Sync {
    fn bound_tool(
        &self,
        session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<Arc<dyn BoundTool>, ToolError>;

    async fn invoke(
        &self,
        session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
        call: ToolCall,
    ) -> Result<Value, ToolError>;
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::{ClosedSchema, EffectKind, InProcessToolRegistry, RecoverySupport, ToolResolver};

    struct TestBoundTool {
        descriptor: CatalogTool,
        invocations: Arc<AtomicUsize>,
    }

    impl private::Sealed for TestBoundTool {}

    #[async_trait]
    impl BoundTool for TestBoundTool {
        fn descriptor(&self) -> &CatalogTool {
            &self.descriptor
        }

        async fn invoke_inner(&self, _call: ToolCall) -> Result<Value, ToolError> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        }

        async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
            Ok(ToolStatus::Succeeded)
        }
    }

    fn tool(name: &str) -> CatalogTool {
        let server_alias = name.split_once('/').expect("test tool is qualified").0;
        CatalogTool::new(
            name,
            server_alias,
            "Returns matching records.",
            ClosedSchema::object(
                "input",
                BTreeMap::from([
                    ("query".to_owned(), ClosedSchema::string()),
                    (
                        "limit".to_owned(),
                        ClosedSchema::nullable(ClosedSchema::integer()),
                    ),
                ]),
            ),
            ClosedSchema::object("output", BTreeMap::new()),
            BTreeSet::new(),
            EffectKind::ReadOnly,
            RecoverySupport::NotRequired,
        )
        .expect("valid descriptor")
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

    #[test]
    fn registry_resolves_only_tools_bound_to_the_requested_session() {
        let registry = InProcessToolRegistry::new();
        let descriptor = tool("docs/search");
        let digest = descriptor.descriptor_digest().to_owned();
        let (bound, _invocations) = test_bound_tool(descriptor);
        registry.register(bound).expect("registry accepts tool");
        let session = SessionId::new(Uuid::nil());
        registry
            .snapshot(
                session.clone(),
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
        assert!(registry.bound_tool(session, "docs/search", &digest).is_ok());
    }

    #[test]
    fn resolver_rejects_a_call_validated_for_another_tool_before_invocation() {
        let (registry, session, second_digest, call, second_invocations) =
            configured_two_tool_registry();

        let result = block_on_ready(registry.invoke(session, "admin/search", &second_digest, call));

        assert!(result.is_err());
        assert_eq!(second_invocations.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn bound_tool_rejects_a_call_validated_for_another_tool_before_invocation() {
        let (registry, session, second_digest, call, second_invocations) =
            configured_two_tool_registry();
        let bound = registry
            .bound_tool(session, "admin/search", &second_digest)
            .expect("second tool is bound to session");

        let result = block_on_ready(bound.invoke(call));

        assert!(result.is_err());
        assert_eq!(second_invocations.load(Ordering::SeqCst), 0);
    }

    fn configured_two_tool_registry() -> (
        InProcessToolRegistry,
        SessionId,
        String,
        ToolCall,
        Arc<AtomicUsize>,
    ) {
        let registry = InProcessToolRegistry::new();
        let first = tool("docs/search");
        let second = tool("admin/search");
        let first_digest = first.descriptor_digest().to_owned();
        let second_digest = second.descriptor_digest().to_owned();
        let session = SessionId::new(Uuid::nil());
        let call = ToolCall::new(
            session.clone(),
            RunId::new(Uuid::nil()),
            FrameId::new(Uuid::nil()),
            PolicyRevision::new(1),
            &first,
            json!({"query": "first", "limit": null}),
            IdempotencyKey::new("call-1").expect("valid idempotency key"),
        )
        .expect("arguments are valid for first tool");
        let (first, _first_invocations) = test_bound_tool(first);
        let (second, second_invocations) = test_bound_tool(second);
        registry.register(first).expect("first tool registers");
        registry.register(second).expect("second tool registers");
        registry
            .snapshot(
                session.clone(),
                Uuid::nil(),
                &BTreeSet::from(["docs/search".to_owned(), "admin/search".to_owned()]),
                &BTreeMap::from([
                    ("docs/search".to_owned(), first_digest.clone()),
                    ("admin/search".to_owned(), second_digest.clone()),
                ]),
            )
            .expect("session snapshot is valid");
        (registry, session, second_digest, call, second_invocations)
    }
}
