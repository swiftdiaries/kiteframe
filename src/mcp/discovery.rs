use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::RwLock;

use crate::PostgresStore;
use crate::{
    BoundTool, Capability, CatalogTool, ClosedSchema, EffectKind, IdempotencyKey, PrincipalId,
    RecoverySupport, Secret, SessionId, ToolCall, ToolCatalogSnapshot, ToolError, ToolStatus,
    TrustedTool, bind_tool,
};

#[derive(Clone, Debug)]
pub struct McpLimits {
    pub max_servers: usize,
    pub max_pages_per_server: usize,
    pub max_tools: usize,
    pub max_descriptor_bytes: usize,
    pub max_catalog_bytes: usize,
}

#[derive(Clone, Debug)]
pub enum McpServerConfig {
    Stdio {
        alias: String,
        program: String,
        args: Vec<String>,
        env: Vec<(String, Secret)>,
    },
    StreamableHttp {
        alias: String,
        url: url::Url,
        headers: Vec<(String, Secret)>,
    },
}

#[derive(Clone, Debug)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub input: ClosedSchema,
    pub output: ClosedSchema,
    pub metadata: Value,
}

pub fn closed_schema(schema: &Value, name: &str) -> Result<ClosedSchema, McpError> {
    let object = schema
        .as_object()
        .ok_or_else(|| McpError::Configuration("tool schema must be an object".to_owned()))?;
    match object.get("type").and_then(Value::as_str) {
        Some("boolean") => Ok(ClosedSchema::boolean()),
        Some("integer") => Ok(ClosedSchema::integer()),
        Some("number") => Ok(ClosedSchema::number()),
        Some("string") => Ok(ClosedSchema::string()),
        Some("array") => Ok(ClosedSchema::array(closed_schema(
            object
                .get("items")
                .ok_or_else(|| McpError::Configuration("array schema needs items".to_owned()))?,
            name,
        )?)),
        Some("object") => closed_object_schema(object, name),
        _ if object.contains_key("enum") => closed_enum_schema(object),
        _ => Err(McpError::Configuration(
            "unsupported tool schema type".to_owned(),
        )),
    }
}

fn closed_object_schema(schema: &Map<String, Value>, name: &str) -> Result<ClosedSchema, McpError> {
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Err(McpError::Configuration(
            "tool object schemas must set additionalProperties false".to_owned(),
        ));
    }
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| McpError::Configuration("tool object schema needs properties".to_owned()))?;
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| McpError::Configuration("tool object schema needs required".to_owned()))?;
    if required.len() != properties.len()
        || required.iter().any(|item| {
            item.as_str()
                .is_none_or(|field| !properties.contains_key(field))
        })
    {
        return Err(McpError::Configuration(
            "tool object properties must all be required".to_owned(),
        ));
    }
    let mut closed = BTreeMap::new();
    for (field, definition) in properties {
        closed.insert(field.clone(), closed_schema(definition, field)?);
    }
    Ok(ClosedSchema::object(name, closed))
}

fn closed_enum_schema(schema: &Map<String, Value>) -> Result<ClosedSchema, McpError> {
    let values = schema
        .get("enum")
        .and_then(Value::as_array)
        .ok_or_else(|| McpError::Configuration("enum must be an array".to_owned()))?;
    let mut closed = BTreeSet::new();
    for value in values {
        let primitive = match value {
            Value::Bool(value) => crate::PrimitiveValue::Boolean(*value),
            Value::Number(value) if value.is_i64() => {
                crate::PrimitiveValue::Integer(value.as_i64().expect("i64 checked"))
            }
            Value::String(value) => crate::PrimitiveValue::String(value.clone()),
            _ => {
                return Err(McpError::Configuration(
                    "enum values must be primitive boolean, integer, or string".to_owned(),
                ));
            }
        };
        closed.insert(primitive);
    }
    if closed.is_empty() {
        return Err(McpError::Configuration("enum must not be empty".to_owned()));
    }
    Ok(ClosedSchema::enumeration(closed))
}

#[async_trait]
pub trait AuthenticatedMcpPeer: Send + Sync {
    async fn list_tools(
        &self,
        cursor: Option<String>,
    ) -> Result<(Vec<RemoteTool>, Option<String>), McpError>;
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError>;
}

pub struct RmcpPeer {
    service: tokio::sync::Mutex<rmcp::service::RunningService<rmcp::RoleClient, ()>>,
}

impl RmcpPeer {
    pub async fn stdio(config: &McpServerConfig) -> Result<Self, McpError> {
        let McpServerConfig::Stdio {
            program, args, env, ..
        } = config
        else {
            return Err(McpError::Configuration(
                "expected stdio configuration".to_owned(),
            ));
        };
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        for (key, value) in env {
            command.env(key, value.expose());
        }
        let transport = rmcp::transport::TokioChildProcess::new(command)
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        let service = rmcp::ServiceExt::serve((), transport)
            .await
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        Ok(Self {
            service: tokio::sync::Mutex::new(service),
        })
    }
    pub async fn streamable_http(config: &McpServerConfig) -> Result<Self, McpError> {
        let McpServerConfig::StreamableHttp { url, headers, .. } = config else {
            return Err(McpError::Configuration(
                "expected streamable HTTP configuration".to_owned(),
            ));
        };
        let mut custom_headers = std::collections::HashMap::new();
        for (name, value) in headers {
            let name = http::header::HeaderName::try_from(name.as_str())
                .map_err(|error| McpError::Configuration(error.to_string()))?;
            let value = http::header::HeaderValue::try_from(value.expose())
                .map_err(|error| McpError::Configuration(error.to_string()))?;
            custom_headers.insert(name, value);
        }
        let transport = rmcp::transport::StreamableHttpClientTransport::from_config(
            rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                url.as_str(),
            )
            .custom_headers(custom_headers),
        );
        let service = rmcp::ServiceExt::serve((), transport)
            .await
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        Ok(Self {
            service: tokio::sync::Mutex::new(service),
        })
    }
}

#[async_trait]
impl AuthenticatedMcpPeer for RmcpPeer {
    async fn list_tools(
        &self,
        cursor: Option<String>,
    ) -> Result<(Vec<RemoteTool>, Option<String>), McpError> {
        let service = self.service.lock().await;
        let result = service
            .peer()
            .list_tools(Some(
                rmcp::model::PaginatedRequestParams::default().with_cursor(cursor),
            ))
            .await
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        let tools = result
            .tools
            .into_iter()
            .map(remote_tool_from_rmcp)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((tools, result.next_cursor))
    }
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        let arguments = arguments
            .as_object()
            .cloned()
            .ok_or_else(|| McpError::Protocol("tool arguments must be an object".to_owned()))?;
        let service = self.service.lock().await;
        let result = service
            .peer()
            .call_tool(
                rmcp::model::CallToolRequestParams::new(name.to_owned()).with_arguments(arguments),
            )
            .await
            .map_err(|error| McpError::Protocol(error.to_string()))?;
        if result.is_error == Some(true) {
            return Err(McpError::Protocol(
                "server returned a tool error".to_owned(),
            ));
        }
        result
            .structured_content
            .ok_or_else(|| McpError::Protocol("tool result lacks structured_content".to_owned()))
    }
}

fn remote_tool_from_rmcp(tool: rmcp::model::Tool) -> Result<RemoteTool, McpError> {
    let input = closed_schema(
        &Value::Object((*tool.input_schema).clone()),
        &format!("{}_input", tool.name),
    )?;
    let output = closed_schema(
        &Value::Object(
            (**tool.output_schema.as_ref().ok_or_else(|| {
                McpError::Configuration("tool outputSchema is required".to_owned())
            })?)
            .clone(),
        ),
        &format!("{}_output", tool.name),
    )?;
    let metadata =
        serde_json::to_value(&tool).map_err(|error| McpError::Protocol(error.to_string()))?;
    Ok(RemoteTool {
        name: tool.name.into_owned(),
        description: tool.description.map_or_else(
            || "MCP tool".to_owned(),
            |description| description.into_owned(),
        ),
        input,
        output,
        metadata,
    })
}

#[derive(Clone, Debug)]
pub struct CatalogRefresh {
    pub epoch: ToolCatalogSnapshot,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("invalid MCP configuration: {0}")]
    Configuration(String),
    #[error("MCP protocol error: {0}")]
    Protocol(String),
    #[error("MCP limit exceeded: {0}")]
    LimitExceeded(String),
    #[error("MCP catalog error: {0}")]
    Catalog(#[from] crate::ToolError),
    #[error("MCP store error: {0}")]
    Store(#[from] crate::StoreError),
    #[error("session is not attached")]
    SessionNotAttached,
}

type SessionConnections = BTreeMap<SessionId, BTreeMap<String, Arc<dyn AuthenticatedMcpPeer>>>;

pub struct McpManager {
    limits: McpLimits,
    store: Option<Arc<PostgresStore>>,
    connections: RwLock<SessionConnections>,
    catalogs: RwLock<BTreeMap<SessionId, ToolCatalogSnapshot>>,
}

impl McpManager {
    #[must_use]
    pub fn new(limits: McpLimits) -> Self {
        Self {
            limits,
            store: None,
            connections: RwLock::new(BTreeMap::new()),
            catalogs: RwLock::new(BTreeMap::new()),
        }
    }
    #[must_use]
    pub fn with_store(limits: McpLimits, store: Arc<PostgresStore>) -> Self {
        Self {
            limits,
            store: Some(store),
            connections: RwLock::new(BTreeMap::new()),
            catalogs: RwLock::new(BTreeMap::new()),
        }
    }
    pub async fn attach(
        &self,
        session: SessionId,
        principal: PrincipalId,
        configs: Vec<McpServerConfig>,
    ) -> Result<ToolCatalogSnapshot, McpError> {
        if configs.is_empty() {
            return Err(McpError::Configuration(
                "at least one MCP server is required".to_owned(),
            ));
        }
        if configs.len() > self.limits.max_servers {
            return Err(McpError::LimitExceeded("server count".to_owned()));
        }
        let mut snapshot = None;
        for config in configs {
            let alias = match &config {
                McpServerConfig::Stdio { alias, .. }
                | McpServerConfig::StreamableHttp { alias, .. } => alias.clone(),
            };
            let peer = match &config {
                McpServerConfig::Stdio { .. } => RmcpPeer::stdio(&config).await?,
                McpServerConfig::StreamableHttp { .. } => {
                    RmcpPeer::streamable_http(&config).await?
                }
            };
            snapshot = Some(
                self.attach_peer(session.clone(), principal.clone(), alias, Arc::new(peer))
                    .await?,
            );
        }
        snapshot.ok_or_else(|| {
            McpError::Configuration("at least one MCP server is required".to_owned())
        })
    }
    pub async fn attach_peer(
        &self,
        session: SessionId,
        _principal: PrincipalId,
        alias: String,
        peer: Arc<dyn AuthenticatedMcpPeer>,
    ) -> Result<ToolCatalogSnapshot, McpError> {
        if alias.is_empty() {
            return Err(McpError::Configuration(
                "server alias must not be empty".to_owned(),
            ));
        }
        let mut connections = self.connections.write().await;
        let servers = connections.entry(session.clone()).or_default();
        if !servers.contains_key(&alias) && servers.len() >= self.limits.max_servers {
            return Err(McpError::LimitExceeded("server count".to_owned()));
        }
        servers.insert(alias, peer);
        drop(connections);
        let snapshot = self.build_snapshot(session.clone()).await?;
        if let Some(store) = &self.store {
            store.append_catalog_epoch(&snapshot).await?;
        }
        self.catalogs
            .write()
            .await
            .insert(session, snapshot.clone());
        Ok(snapshot)
    }
    pub async fn refresh(
        &self,
        session: SessionId,
        _server_alias: &str,
    ) -> Result<CatalogRefresh, McpError> {
        let before = self
            .catalogs
            .read()
            .await
            .get(&session)
            .cloned()
            .ok_or(McpError::SessionNotAttached)?;
        let epoch = self.build_snapshot(session.clone()).await?;
        if let Some(store) = &self.store {
            store.append_catalog_epoch(&epoch).await?;
        }
        self.catalogs.write().await.insert(session, epoch.clone());
        let before_tools = before.tools();
        let after_tools = epoch.tools();
        let added = after_tools
            .keys()
            .filter(|name| !before_tools.contains_key(*name))
            .cloned()
            .collect();
        let removed = before_tools
            .keys()
            .filter(|name| !after_tools.contains_key(*name))
            .cloned()
            .collect();
        let changed = after_tools
            .iter()
            .filter(|(name, tool)| {
                before_tools
                    .get(*name)
                    .is_some_and(|old| old.descriptor_digest() != tool.descriptor_digest())
            })
            .map(|(name, _)| name.clone())
            .collect();
        Ok(CatalogRefresh {
            epoch,
            added,
            removed,
            changed,
        })
    }
    pub async fn bound_tool(
        &self,
        session: SessionId,
        qualified_name: &str,
        digest: &str,
    ) -> Result<Arc<dyn BoundTool>, McpError> {
        let catalog = self
            .catalogs
            .read()
            .await
            .get(&session)
            .cloned()
            .ok_or(McpError::SessionNotAttached)?;
        let descriptor = catalog.resolve(qualified_name, digest)?.clone();
        let alias = descriptor.server_alias().to_owned();
        let peer = self
            .connections
            .read()
            .await
            .get(&session)
            .and_then(|servers| servers.get(&alias))
            .cloned()
            .ok_or(McpError::SessionNotAttached)?;
        Ok(bind_tool(Arc::new(McpTrustedTool {
            session,
            descriptor,
            peer,
        })))
    }
    async fn build_snapshot(&self, session: SessionId) -> Result<ToolCatalogSnapshot, McpError> {
        let connections = self.connections.read().await;
        let servers = connections
            .get(&session)
            .ok_or(McpError::SessionNotAttached)?;
        let peers = servers
            .iter()
            .map(|(alias, peer)| (alias.clone(), Arc::clone(peer)))
            .collect::<Vec<_>>();
        drop(connections);
        let mut tools = Vec::new();
        let mut total_bytes = 0;
        for (alias, peer) in peers {
            let mut cursor = None;
            for page in 0..self.limits.max_pages_per_server {
                let (remote, next) = peer.list_tools(cursor.take()).await?;
                for raw in remote {
                    if tools.len() >= self.limits.max_tools {
                        return Err(McpError::LimitExceeded("tool count".to_owned()));
                    }
                    let bytes = serde_json::to_vec(&raw.metadata)
                        .map_err(|e| McpError::Protocol(e.to_string()))?
                        .len();
                    if bytes > self.limits.max_descriptor_bytes {
                        return Err(McpError::LimitExceeded("descriptor bytes".to_owned()));
                    }
                    total_bytes += bytes;
                    if total_bytes > self.limits.max_catalog_bytes {
                        return Err(McpError::LimitExceeded("catalog bytes".to_owned()));
                    }
                    let mut capabilities = BTreeSet::new();
                    capabilities.insert(Capability::McpUnknown);
                    tools.push(CatalogTool::new(
                        format!("{alias}/{}", raw.name),
                        alias.clone(),
                        raw.description,
                        raw.input,
                        raw.output,
                        capabilities,
                        EffectKind::ReadOnly,
                        RecoverySupport::NotRequired,
                    )?);
                }
                match next {
                    Some(value) => cursor = Some(value),
                    None => break,
                }
                if page + 1 == self.limits.max_pages_per_server {
                    return Err(McpError::LimitExceeded("page count".to_owned()));
                }
            }
        }
        ToolCatalogSnapshot::new(session, uuid::Uuid::now_v7(), tools).map_err(McpError::Catalog)
    }
}

struct McpTrustedTool {
    session: SessionId,
    descriptor: CatalogTool,
    peer: Arc<dyn AuthenticatedMcpPeer>,
}
#[async_trait]
impl TrustedTool for McpTrustedTool {
    fn descriptor(&self) -> &CatalogTool {
        &self.descriptor
    }
    async fn invoke(&self, call: ToolCall) -> Result<Value, ToolError> {
        if call.session_id != self.session {
            return Err(ToolError::UnknownSession(call.session_id));
        }
        let output = self
            .peer
            .call_tool(
                self.descriptor
                    .qualified_name()
                    .split_once('/')
                    .map_or(self.descriptor.qualified_name(), |(_, name)| name),
                call.arguments().as_json().clone(),
            )
            .await
            .map_err(|_| ToolError::InvocationFailed)?;
        self.descriptor
            .output()
            .validate(&output)
            .map_err(ToolError::InvalidArguments)?;
        Ok(output)
    }
    async fn status(&self, _key: &IdempotencyKey) -> Result<ToolStatus, ToolError> {
        Err(ToolError::InvocationFailed)
    }
}
