pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod domain;
pub mod dsl;
pub mod hash;
pub mod mcp;
pub mod model;
pub mod policy;
pub mod schema;
pub mod store;
pub mod tools;

pub use domain::{
    Capability, CatalogEpochId, FrameId, IdempotencyKey, PermissionRequestId, PolicyRevision,
    PrincipalId, RunId, Secret, SessionId,
};
pub use hash::{HashError, canonical_json, canonical_sha256};
pub use mcp::{
    AuthenticatedMcpPeer, CatalogRefresh, McpError, McpLimits, McpManager, McpServerConfig,
    RemoteTool, RmcpPeer, closed_schema,
};
pub use model::{
    ModelClient, ModelError, ModelErrorKind, ModelRequest, ModelResponse, ModelResponseItemKind,
    ResponsesClient, ResponsesConfig,
};
pub use policy::{
    CedarAuthorizer, OneOfPrimitive, PolicyDecision, PolicyEffect, PolicyError, PolicyRequest,
    PolicyRule, PolicyTarget, Primitive, SessionAuthorizer, SessionPolicy, TypedAuthorizer,
};
pub use schema::{ClosedSchema, PrimitiveValue, SchemaError};
pub use store::{
    Bindings, CreateSession, FrameCommand, ModelAttempt, NewPermissionRequest, PolicyUpdate, PostgresStore,
    PublishedDefinition, Question, RunState, RunView, SessionView, StartRun, StoreError, ToolCallIntent,
};
pub use tools::{
    BoundTool, CatalogTool, EffectKind, InProcessToolRegistry, RecoverySupport, ToolCall,
    ToolCatalogSnapshot, ToolError, ToolFailure, ToolResolver, ToolStatus, TrustedTool,
    ValidatedArguments, bind_tool,
};
