mod application;
pub mod transport;
mod types;
pub mod v1;
#[cfg(feature = "acp-v2")]
pub mod v2;

pub use application::Application;
pub use transport::{
    AuthError, AuthenticatedConnection, ConnectionAuthenticator, NdjsonRequest, ProtocolConnection,
    StaticBearerAuthenticator, TransportKind, serve_ndjson,
};
pub use types::{
    AGENT_RUNTIME_UNAVAILABLE, ApplicationCommand, ApplicationContext, ApplicationResult,
    ApplicationSession, CloseSession, EventCursor, GetRun, GetUserTask, NegotiatedProtocol,
    NewSession, PermissionResponseContext, ProtocolAdapter, ProtocolError, ProtocolNegotiation,
    PublishDefinition, ResumeSession, UpdateSessionPolicy,
};
pub(crate) use types::{command_session, encode_application_result};
pub use v1::V1Adapter;
#[cfg(feature = "acp-v2")]
pub use v2::V2Adapter;
