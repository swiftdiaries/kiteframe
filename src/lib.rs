pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent;
pub mod config;
pub mod domain;
pub mod dsl;
pub mod engine;
pub mod goal;
pub mod harness;
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

pub use agent::{
    AgentCommandId, AgentKernel, AgentLifecycleAction, AgentLifecycleAuthorizer,
    AgentPolicyDecision, AgentRuntime, AgentRuntimeError, BlockGoal, ClaimAgentCommand,
    CompleteAgentCommand, CompleteGoal, FailAgentCommand, FailTask, GoalCriterionProjection,
    GoalProjection, HarnessStructuralLimits, HarnessTool, InvokeHarness, KernelCommand,
    KernelCommandPayload, KiteAgentRuntime, KiteFrameId, KiteFrameSnapshot, KiteFrameState,
    KiteRuntimeState, RequestApproval, RequestContext, RuntimeCeilings, RuntimeCommandKey,
    RuntimeDisposition, RuntimeEvent, RuntimeHandle, RuntimeHistoryItem, RuntimeHistoryKind,
    RuntimeStart, RuntimeStateSnapshot, RuntimeTransition, TaskOwnerAuthorizer, WaitForEvent,
    validate_transition,
};
pub use config::{ConfigError, HarnessConfig, HarnessMode};
pub use domain::{
    Capability, CatalogEpochId, FrameId, IdempotencyKey, PermissionRequestId, PolicyRevision,
    PrincipalId, RunId, Secret, SessionId, UserTaskId,
};
pub use dsl::ModelRole;
pub use engine::{
    AgentCommandDispatcher, AnswerQuestion, CancelRun, DirectLoopHarnessExecutor, Engine,
    EngineConfig, EngineError, HarnessCommandExecutor, HarnessExecutionError,
    HarnessExecutionResult, PermissionChoice, RecoveryReport, ResolvePermission, RetryRecovery,
};
pub use goal::{
    CriterionAssertion, EvidenceId, EvidenceRecord, EvidenceSourceIdentity, GoalBlocker,
    GoalContract, GoalContractError, GoalPhase, SuccessCriterion, goal_edge_allowed,
};
pub use harness::{
    DenyAllHarnessApprovals, HarnessApprovalBroker, HarnessEventEnvelope, HarnessEventSink,
    HarnessPlugin, HarnessRegistry, HarnessRequestEnvelope, PostgresHarnessEventSink,
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
    CedarAuthorizer, OneOfPrimitive, PersistedAuthorizer, PolicyDecision, PolicyEffect,
    PolicyError, PolicyRequest, PolicyRule, PolicyTarget, Primitive, SessionAuthorizer,
    SessionPolicy, TypedAuthorizer,
};
#[cfg(feature = "acp-v2")]
pub use protocol::V2Adapter;
pub use protocol::{
    AGENT_RUNTIME_UNAVAILABLE, AgentEventCursor, Application, ApplicationCommand,
    ApplicationContext, ApplicationResult, ApplicationSession, AuthenticatedConnection,
    CloseSession, EventCursor, GetAgentExecution, GetRun, GetUserTask, NegotiatedProtocol,
    NewSession, PermissionResponseContext, ProtocolAdapter, ProtocolConnection, ProtocolError,
    ProtocolNegotiation, PublishDefinition, ResumeSession, StaticBearerAuthenticator,
    TransportKind, UpdateSessionPolicy, V1Adapter, serve_ndjson,
};
pub use schema::{ClosedSchema, PrimitiveValue, SchemaError};
pub use store::{
    AcceptedUserTask, AgentCommandContext, AgentCommandState, AgentCommandView,
    AgentRecoveryReport, AgentStartCommit, AgentStartSeed, AgentTaskEvent, AgentTaskEventKind,
    Bindings, CreateSession, FrameCommand, ModelAttempt, NewChild, NewPermissionRequest,
    PendingPermission, PolicyUpdate, PostgresStore, PublishedDefinition, Question,
    RecoverableToolCall, RunEvent, RunState, RunToolBinding, RunView, RunnableFrame, SessionView,
    StartRun, StoreError, StoreLimits, SuspensionFrame, ToolCallIntent,
};
pub use task::{
    AgentExecutionView, AgentSelector, CancelUserTask, HarnessRole, OperationalStatus,
    PauseUserTask, ResumeUserTask, StartAgent, SubmitUserTask, TaskLifecycle, TaskPreferences,
    TaskTarget, UserTaskError,
};
pub use tools::{
    BoundTool, CatalogTool, EffectKind, InProcessToolRegistry, RecoverySupport, ToolCall,
    ToolCatalogSnapshot, ToolError, ToolFailure, ToolResolver, ToolStatus, TrustedTool,
    ValidatedArguments, bind_tool,
};
