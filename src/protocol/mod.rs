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
    ApplicationCommand, ApplicationContext, ApplicationResult, ApplicationSession, CloseSession,
    EventCursor, GetRun, NegotiatedProtocol, NewSession, PermissionResponseContext,
    ProtocolAdapter, ProtocolError, ProtocolNegotiation, PublishWorkflow, ResumeSession,
    UpdateSessionPolicy,
};
pub(crate) use types::{command_session, encode_application_result};
pub use v1::V1Adapter;
#[cfg(feature = "acp-v2")]
pub use v2::V2Adapter;
