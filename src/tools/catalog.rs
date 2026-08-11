use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use crate::{Capability, CatalogEpochId, ClosedSchema, SessionId, canonical_sha256};

use super::{BoundTool, CatalogTool, EffectKind, RecoverySupport, ToolError, ToolResolver};

#[derive(Clone, Debug)]
pub struct ToolCatalogSnapshot {
    pub session_id: SessionId,
    pub epoch_id: CatalogEpochId,
    pub digest: String,
    pub tools: BTreeMap<String, CatalogTool>,
}

impl ToolCatalogSnapshot {
    pub fn new(
        session_id: SessionId,
        epoch_id: uuid::Uuid,
        tools: impl IntoIterator<Item = CatalogTool>,
    ) -> Result<Self, ToolError> {
        let mut entries = BTreeMap::new();
        for tool in tools {
            tool.verify_descriptor_digest()?;
            let name = tool.qualified_name.clone();
            if entries.insert(name.clone(), tool).is_some() {
                return Err(ToolError::DuplicateQualifiedName(name));
            }
        }
        let digest = canonical_sha256(
            &entries
                .iter()
                .map(|(name, tool)| (name, tool.normalized_descriptor()))
                .collect::<Vec<_>>(),
        )
        .map_err(ToolError::Canonicalization)?;
        Ok(Self {
            session_id,
            epoch_id: CatalogEpochId::new(epoch_id),
            digest,
            tools: entries,
        })
    }

    pub fn resolve(
        &self,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<&CatalogTool, ToolError> {
        let tool = self
            .tools
            .get(qualified_name)
            .ok_or_else(|| ToolError::UnknownTool(qualified_name.to_owned()))?;
        if tool.descriptor_digest == descriptor_digest {
            Ok(tool)
        } else {
            Err(ToolError::ToolContractChanged {
                qualified_name: qualified_name.to_owned(),
            })
        }
    }

    /// Applies the two independent visibility constraints before a tool can be model-visible.
    /// A binding pins both the qualified name and the immutable descriptor digest.
    pub fn model_visible_tools(
        &self,
        principal_visible: &BTreeSet<String>,
        workflow_bindings: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, CatalogTool>, ToolError> {
        let mut tools = BTreeMap::new();
        for name in principal_visible {
            let Some(expected_digest) = workflow_bindings.get(name) else {
                continue;
            };
            let descriptor = self.resolve(name, expected_digest)?;
            tools.insert(name.clone(), descriptor.clone());
        }
        Ok(tools)
    }
}

/// Deployment-owned registry for built-ins. It implements the same contracts as MCP tools,
/// but the built-ins are never represented as MCP servers.
#[derive(Default)]
pub struct InProcessToolRegistry {
    tools: RwLock<BTreeMap<String, Arc<dyn BoundTool>>>,
}

impl InProcessToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, tool: Arc<dyn BoundTool>) -> Result<(), ToolError> {
        tool.descriptor().verify_descriptor_digest()?;
        let mut tools = self.tools.write().expect("tool registry lock poisoned");
        let name = tool.descriptor().qualified_name.clone();
        if tools.contains_key(&name) {
            return Err(ToolError::DuplicateQualifiedName(name));
        }
        tools.insert(name, tool);
        Ok(())
    }

    pub fn snapshot(
        &self,
        session_id: SessionId,
        epoch_id: uuid::Uuid,
    ) -> Result<ToolCatalogSnapshot, ToolError> {
        let tools = self.tools.read().expect("tool registry lock poisoned");
        ToolCatalogSnapshot::new(
            session_id,
            epoch_id,
            tools.values().map(|tool| tool.descriptor().clone()),
        )
    }

    pub fn unknown_mcp_tool(
        &self,
        server_alias: &str,
        tool_name: &str,
        description: &str,
        input: ClosedSchema,
        output: ClosedSchema,
    ) -> Result<CatalogTool, ToolError> {
        CatalogTool::new(
            format!("{server_alias}/{tool_name}"),
            server_alias,
            description,
            input,
            output,
            BTreeSet::from([Capability::McpUnknown]),
            EffectKind::ReadOnly,
            RecoverySupport::NotRequired,
        )
    }
}

impl ToolResolver for InProcessToolRegistry {
    fn bound_tool(
        &self,
        _session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<Arc<dyn BoundTool>, ToolError> {
        let tools = self.tools.read().expect("tool registry lock poisoned");
        let tool = tools
            .get(qualified_name)
            .ok_or_else(|| ToolError::UnknownTool(qualified_name.to_owned()))?;
        if tool.descriptor().descriptor_digest != descriptor_digest {
            return Err(ToolError::ToolContractChanged {
                qualified_name: qualified_name.to_owned(),
            });
        }
        Ok(Arc::clone(tool))
    }
}
