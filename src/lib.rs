pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod config;
pub mod domain;
pub mod dsl;
pub mod engine;
pub mod goal;
pub mod hash;
pub mod mcp;
pub mod model;
pub mod policy;
pub mod protocol;
pub mod schema;
#[allow(deprecated)]
pub mod store;
pub mod task;
pub mod tools;

pub use config::{ConfigError, HarnessConfig, HarnessMode};
pub use domain::{
    Capability, CatalogEpochId, FrameId, IdempotencyKey, PermissionRequestId, PolicyRevision,
    PrincipalId, RunId, Secret, SessionId, UserTaskId,
};
pub use engine::{
    AnswerQuestion, CancelRun, Engine, EngineConfig, EngineError, PermissionChoice, RecoveryReport,
    ResolvePermission, RetryRecovery,
};
pub use goal::{GoalContract, GoalContractError, SuccessCriterion};
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
    CedarAuthorizer, OneOfPrimitive, PersistedAuthorizer, PolicyDecision, PolicyEffect,
    PolicyError, PolicyRequest, PolicyRule, PolicyTarget, Primitive, SessionAuthorizer,
    SessionPolicy, TypedAuthorizer,
};
#[cfg(feature = "acp-v2")]
pub use protocol::V2Adapter;
pub use protocol::{
    AGENT_RUNTIME_UNAVAILABLE, Application, ApplicationCommand, ApplicationContext,
    ApplicationResult, ApplicationSession, AuthenticatedConnection, CloseSession, EventCursor,
    GetRun, GetUserTask, NegotiatedProtocol, NewSession, PermissionResponseContext,
    ProtocolAdapter, ProtocolConnection, ProtocolError, ProtocolNegotiation, PublishDefinition,
    ResumeSession, StaticBearerAuthenticator, TransportKind, UpdateSessionPolicy, V1Adapter,
    serve_ndjson,
};
pub use schema::{ClosedSchema, PrimitiveValue, SchemaError};
pub use store::{
    AcceptedUserTask, Bindings, CreateSession, FrameCommand, ModelAttempt, NewChild,
    NewPermissionRequest, PendingPermission, PolicyUpdate, PostgresStore, PublishedDefinition,
    Question, RecoverableToolCall, RunEvent, RunState, RunToolBinding, RunView, RunnableFrame,
    SessionView, StartRun, StoreError, StoreLimits, SuspensionFrame, ToolCallIntent,
};
pub use task::{
    AgentSelector, HarnessRole, SubmitUserTask, TaskPreferences, TaskTarget, UserTaskError,
};
pub use tools::{
    BoundTool, CatalogTool, EffectKind, InProcessToolRegistry, RecoverySupport, ToolCall,
    ToolCatalogSnapshot, ToolError, ToolFailure, ToolResolver, ToolStatus, TrustedTool,
    ValidatedArguments, bind_tool,
};
