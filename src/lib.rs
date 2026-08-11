pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod domain;
pub mod dsl;
pub mod hash;
pub mod model;
pub mod schema;
pub mod tools;

pub use domain::{
    Capability, CatalogEpochId, FrameId, IdempotencyKey, PermissionRequestId, PolicyRevision,
    PrincipalId, RunId, Secret, SessionId,
};
pub use hash::{HashError, canonical_json, canonical_sha256};
pub use model::{
    ModelClient, ModelError, ModelErrorKind, ModelRequest, ModelResponse, ModelResponseItemKind,
    ResponsesClient, ResponsesConfig,
};
pub use schema::{ClosedSchema, PrimitiveValue, SchemaError};
pub use tools::{
    BoundTool, CatalogTool, EffectKind, InProcessToolRegistry, RecoverySupport, ToolCall,
    ToolCatalogSnapshot, ToolError, ToolFailure, ToolResolver, ToolStatus, TrustedTool,
    ValidatedArguments, bind_tool,
};
