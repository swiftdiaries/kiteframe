pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod config;
pub mod domain;
pub mod dsl;
pub mod engine;
pub mod hash;
pub mod mcp;
pub mod model;
pub mod policy;
pub mod protocol;
pub mod schema;
pub mod store;
pub mod tools;

pub use domain::{
    Capability, CatalogEpochId, FrameId, IdempotencyKey, PermissionRequestId, PolicyRevision,
    PrincipalId, RunId, Secret, SessionId,
};
pub use config::{ConfigError, HarnessConfig, HarnessMode};
pub use hash::{HashError, canonical_json, canonical_sha256};
pub use engine::{AnswerQuestion, CancelRun, Engine, EngineConfig, EngineError, PermissionChoice, RecoveryReport, ResolvePermission};
pub use mcp::{
    AuthenticatedMcpPeer, CatalogRefresh, McpError, McpLimits, McpManager, McpServerConfig,
    RemoteTool, RmcpPeer, closed_schema,
};
pub use model::{
    ModelClient, ModelError, ModelErrorKind, ModelRequest, ModelResponse, ModelResponseItemKind,
    ResponsesClient, ResponsesConfig,
};
pub use policy::{
    CedarAuthorizer, PersistedAuthorizer, OneOfPrimitive, PolicyDecision, PolicyEffect, PolicyError, PolicyRequest,
    PolicyRule, PolicyTarget, Primitive, SessionAuthorizer, SessionPolicy, TypedAuthorizer,
};
pub use protocol::{Application, ApplicationCommand, ApplicationContext, ApplicationResult, AuthenticatedConnection, EventCursor, GetRun, ProtocolError, PublishWorkflow, StaticBearerAuthenticator, TransportKind, UpdateSessionPolicy, serve_ndjson};
pub use schema::{ClosedSchema, PrimitiveValue, SchemaError};
pub use store::{
    Bindings, CreateSession, FrameCommand, ModelAttempt, NewChild, NewPermissionRequest, PolicyUpdate, PostgresStore,
    PendingPermission, PublishedDefinition, Question, RecoverableToolCall, RunEvent, RunState, RunToolBinding, RunView, RunnableFrame, SessionView, StartRun, StoreError, SuspensionFrame, ToolCallIntent,
};
pub use tools::{
    BoundTool, CatalogTool, EffectKind, InProcessToolRegistry, RecoverySupport, ToolCall,
    ToolCatalogSnapshot, ToolError, ToolFailure, ToolResolver, ToolStatus, TrustedTool,
    ValidatedArguments, bind_tool,
};
