mod discovery;

pub use discovery::{
    AuthenticatedMcpPeer, CatalogRefresh, McpError, McpLimits, McpManager, McpServerConfig,
    RemoteTool, RmcpPeer, closed_schema,
};
