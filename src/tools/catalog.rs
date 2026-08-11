use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use crate::{Capability, CatalogEpochId, ClosedSchema, SessionId, canonical_sha256};

use super::{BoundTool, CatalogTool, EffectKind, RecoverySupport, ToolError, ToolResolver};

#[derive(Clone, Debug)]
pub struct ToolCatalogSnapshot {
    session_id: SessionId,
    epoch_id: CatalogEpochId,
    digest: String,
    tools: BTreeMap<String, CatalogTool>,
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
            let name = tool.qualified_name().to_owned();
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

    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    pub const fn epoch_id(&self) -> &CatalogEpochId {
        &self.epoch_id
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub const fn tools(&self) -> &BTreeMap<String, CatalogTool> {
        &self.tools
    }

    pub fn resolve(
        &self,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<&CatalogTool, ToolError> {
        self.verify_snapshot_digest(qualified_name)?;
        let tool = self
            .tools
            .get(qualified_name)
            .ok_or_else(|| ToolError::UnknownTool(qualified_name.to_owned()))?;
        tool.verify_descriptor_digest()?;
        if tool.descriptor_digest() == descriptor_digest {
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

    fn verify_snapshot_digest(&self, qualified_name: &str) -> Result<(), ToolError> {
        let actual = canonical_sha256(
            &self
                .tools
                .iter()
                .map(|(name, tool)| (name, tool.normalized_descriptor()))
                .collect::<Vec<_>>(),
        )
        .map_err(ToolError::Canonicalization)?;
        if self.digest == actual {
            Ok(())
        } else {
            Err(ToolError::ToolContractChanged {
                qualified_name: qualified_name.to_owned(),
            })
        }
    }
}

/// Deployment-owned registry for built-ins. It implements the same contracts as MCP tools,
/// but the built-ins are never represented as MCP servers.
#[derive(Default)]
pub struct InProcessToolRegistry {
    tools: RwLock<BTreeMap<String, Arc<dyn BoundTool>>>,
    sessions: RwLock<BTreeMap<SessionId, ToolCatalogSnapshot>>,
}

impl InProcessToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, tool: Arc<dyn BoundTool>) -> Result<(), ToolError> {
        tool.descriptor().verify_descriptor_digest()?;
        let mut tools = self.tools.write().expect("tool registry lock poisoned");
        let name = tool.descriptor().qualified_name().to_owned();
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
        principal_visible: &BTreeSet<String>,
        workflow_bindings: &BTreeMap<String, String>,
    ) -> Result<ToolCatalogSnapshot, ToolError> {
        let tools = self.tools.read().expect("tool registry lock poisoned");
        let catalog = ToolCatalogSnapshot::new(
            session_id.clone(),
            epoch_id,
            tools.values().map(|tool| tool.descriptor().clone()),
        )?;
        let visible = catalog.model_visible_tools(principal_visible, workflow_bindings)?;
        let snapshot =
            ToolCatalogSnapshot::new(session_id.clone(), epoch_id, visible.into_values())?;
        self.sessions
            .write()
            .expect("session registry lock poisoned")
            .insert(session_id, snapshot.clone());
        Ok(snapshot)
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
        session: SessionId,
        qualified_name: &str,
        descriptor_digest: &str,
    ) -> Result<Arc<dyn BoundTool>, ToolError> {
        let snapshot = self
            .sessions
            .read()
            .expect("session registry lock poisoned")
            .get(&session)
            .cloned()
            .ok_or_else(|| ToolError::UnknownSession(session.clone()))?;
        snapshot.resolve(qualified_name, descriptor_digest)?;

        let tools = self.tools.read().expect("tool registry lock poisoned");
        let tool = tools
            .get(qualified_name)
            .ok_or_else(|| ToolError::UnknownTool(qualified_name.to_owned()))?;
        tool.descriptor().verify_descriptor_digest()?;
        if tool.descriptor().descriptor_digest() != descriptor_digest {
            return Err(ToolError::ToolContractChanged {
                qualified_name: qualified_name.to_owned(),
            });
        }
        Ok(Arc::clone(tool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_rejects_a_descriptor_replaced_after_snapshot_creation() {
        let original = CatalogTool::new(
            "docs/search",
            "docs",
            "Returns matching records.",
            ClosedSchema::object("input", BTreeMap::new()),
            ClosedSchema::object("output", BTreeMap::new()),
            BTreeSet::new(),
            EffectKind::ReadOnly,
            RecoverySupport::NotRequired,
        )
        .expect("valid descriptor");
        let digest = original.descriptor_digest().to_owned();
        let mut snapshot = ToolCatalogSnapshot::new(
            SessionId::new(uuid::Uuid::nil()),
            uuid::Uuid::nil(),
            [original],
        )
        .expect("valid snapshot");
        let replacement = CatalogTool::new(
            "docs/search",
            "docs",
            "Returns changed records.",
            ClosedSchema::object("input", BTreeMap::new()),
            ClosedSchema::object("output", BTreeMap::new()),
            BTreeSet::new(),
            EffectKind::ReadOnly,
            RecoverySupport::NotRequired,
        )
        .expect("valid replacement descriptor");
        snapshot.tools.insert("docs/search".to_owned(), replacement);

        assert!(snapshot.resolve("docs/search", &digest).is_err());
    }
}
