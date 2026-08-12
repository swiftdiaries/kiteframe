use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Capability, ClosedSchema, canonical_sha256};

use super::ToolError;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    ReadOnly,
    Effectful,
}

impl EffectKind {
    const fn normalized_name(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Effectful => "effectful",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoverySupport {
    NotRequired,
    Status,
    Compensation,
    Unsupported,
}

impl RecoverySupport {
    const fn normalized_name(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Status => "status",
            Self::Compensation => "compensation",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CatalogTool {
    qualified_name: String,
    server_alias: String,
    description: String,
    input: ClosedSchema,
    output: ClosedSchema,
    descriptor_digest: String,
    capabilities: BTreeSet<Capability>,
    effect: EffectKind,
    recovery: RecoverySupport,
}

impl CatalogTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        qualified_name: impl Into<String>,
        server_alias: impl Into<String>,
        description: impl Into<String>,
        input: ClosedSchema,
        output: ClosedSchema,
        capabilities: BTreeSet<Capability>,
        effect: EffectKind,
        recovery: RecoverySupport,
    ) -> Result<Self, ToolError> {
        let mut tool = Self {
            qualified_name: qualified_name.into(),
            server_alias: server_alias.into(),
            description: description.into(),
            input,
            output,
            descriptor_digest: String::new(),
            capabilities,
            effect,
            recovery,
        };
        tool.validate()?;
        tool.descriptor_digest =
            canonical_sha256(&tool.normalized_descriptor()).map_err(ToolError::Canonicalization)?;
        Ok(tool)
    }

    pub fn from_normalized(value: &Value) -> Result<Self, ToolError> {
        #[derive(Deserialize)]
        struct StoredTool {
            qualified_name: String,
            server_alias: String,
            description: String,
            input: ClosedSchema,
            output: ClosedSchema,
            capabilities: BTreeSet<Capability>,
            effect: EffectKind,
            recovery: RecoverySupport,
        }
        let stored: StoredTool = serde_json::from_value(value.clone())
            .map_err(|error| ToolError::InvalidStoredDescriptor(error.to_string()))?;
        Self::new(
            stored.qualified_name,
            stored.server_alias,
            stored.description,
            stored.input,
            stored.output,
            stored.capabilities,
            stored.effect,
            stored.recovery,
        )
    }

    pub fn validate(&self) -> Result<(), ToolError> {
        let (alias, name) = self
            .qualified_name
            .split_once('/')
            .ok_or_else(|| ToolError::InvalidQualifiedName(self.qualified_name.clone()))?;
        if alias.is_empty()
            || name.is_empty()
            || name.contains('/')
            || alias != self.server_alias
            || self.description.trim().is_empty()
        {
            return Err(ToolError::InvalidQualifiedName(self.qualified_name.clone()));
        }
        self.input
            .validate_definition()
            .map_err(ToolError::UnsupportedSchema)?;
        self.output
            .validate_definition()
            .map_err(ToolError::UnsupportedSchema)?;
        if self.effect == EffectKind::Effectful
            && !matches!(
                self.recovery,
                RecoverySupport::Status | RecoverySupport::Compensation
            )
        {
            return Err(ToolError::UnsafeEffectRecovery(self.qualified_name.clone()));
        }
        Ok(())
    }

    pub fn verify_descriptor_digest(&self) -> Result<(), ToolError> {
        self.validate()?;
        let actual =
            canonical_sha256(&self.normalized_descriptor()).map_err(ToolError::Canonicalization)?;
        if self.descriptor_digest == actual {
            Ok(())
        } else {
            Err(ToolError::ToolContractChanged {
                qualified_name: self.qualified_name.clone(),
            })
        }
    }

    #[must_use]
    pub fn normalized_descriptor(&self) -> Value {
        json!({
            "qualified_name": self.qualified_name,
            "server_alias": self.server_alias,
            "description": self.description,
            "input": self.input.normalized_json(),
            "output": self.output.normalized_json(),
            "capabilities": self.capabilities.iter().map(|capability| match capability {
                Capability::McpUnknown => "mcp_unknown",
                Capability::ReadData => "read_data",
                Capability::WriteData => "write_data",
                Capability::Network => "network",
                Capability::Filesystem => "filesystem",
            }).collect::<Vec<_>>(),
            "effect": self.effect.normalized_name(),
            "recovery": self.recovery.normalized_name(),
        })
    }

    #[must_use]
    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    #[must_use]
    pub fn server_alias(&self) -> &str {
        &self.server_alias
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub const fn input(&self) -> &ClosedSchema {
        &self.input
    }

    #[must_use]
    pub const fn output(&self) -> &ClosedSchema {
        &self.output
    }

    #[must_use]
    pub fn descriptor_digest(&self) -> &str {
        &self.descriptor_digest
    }

    #[must_use]
    pub const fn capabilities(&self) -> &BTreeSet<Capability> {
        &self.capabilities
    }

    #[must_use]
    pub const fn effect(&self) -> EffectKind {
        self.effect
    }

    #[must_use]
    pub const fn recovery(&self) -> RecoverySupport {
        self.recovery
    }
}
