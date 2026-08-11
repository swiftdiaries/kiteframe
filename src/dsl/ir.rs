use std::collections::BTreeMap;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use crate::{CatalogEpochId, CatalogTool};

/// Deployment ceilings for author-supplied workflow budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub steps: u64,
    pub model_turns_per_task: u64,
    pub tool_calls_per_response: u64,
    pub parallel_branches: u64,
    pub response_item_bytes: u64,
    pub active_execution_seconds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            steps: u64::MAX,
            model_turns_per_task: u64::MAX,
            tool_calls_per_response: u64::MAX,
            parallel_branches: u64::MAX,
            response_item_bytes: u64::MAX,
            active_execution_seconds: u64::MAX,
        }
    }
}

/// A descriptor binding captured from a principal-scoped catalog at publication time.
#[derive(Clone, Debug)]
pub struct CompiledToolBinding {
    pub qualified_name: String,
    pub catalog_epoch_id: CatalogEpochId,
    pub descriptor_digest: String,
    pub descriptor: CatalogTool,
}

impl Serialize for CompiledToolBinding {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("CompiledToolBinding", 4)?;
        state.serialize_field("qualified_name", &self.qualified_name)?;
        state.serialize_field("catalog_epoch_id", &self.catalog_epoch_id)?;
        state.serialize_field("descriptor_digest", &self.descriptor_digest)?;
        state.serialize_field("descriptor", &self.descriptor.normalized_descriptor())?;
        state.end()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "item")]
pub enum CompiledType {
    String,
    Integer,
    Number,
    Boolean,
    Record(String),
    Optional(Box<Self>),
    List(Box<Self>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum CompiledValue {
    Binding(String),
    String(String),
    Number(String),
    Boolean(bool),
    Null,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompiledCall {
    pub task: String,
    pub arguments: BTreeMap<String, CompiledValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompiledCallBinding {
    pub binding: String,
    pub call: CompiledCall,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum CompiledStatement {
    Call {
        binding: String,
        call: CompiledCall,
    },
    Parallel {
        calls: Vec<CompiledCallBinding>,
    },
    Ask {
        binding: String,
        answer_type: CompiledType,
        question: CompiledValue,
    },
    Goto {
        call: CompiledCall,
    },
    Done {
        value: CompiledValue,
    },
    Fail {
        code: CompiledValue,
        message: CompiledValue,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompiledOutcome {
    pub fields: BTreeMap<String, CompiledType>,
    pub statements: Vec<CompiledStatement>,
}

/// The closed, tagged-union wire contract for the internal `complete_task` tool.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompletionContract {
    pub outcomes: BTreeMap<String, BTreeMap<String, CompiledType>>,
    pub required_nullable_fields: BTreeMap<String, CompiledType>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompiledTask {
    pub inputs: BTreeMap<String, CompiledType>,
    pub result: CompiledType,
    pub prompt: String,
    pub tools: Vec<CompiledToolBinding>,
    pub outcomes: BTreeMap<String, CompiledOutcome>,
    pub completion: CompletionContract,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompiledWorkflow {
    pub dsl_version: u8,
    pub name: String,
    pub source: String,
    pub definition_hash: String,
    pub inputs: BTreeMap<String, CompiledType>,
    pub result: CompiledType,
    pub entry: CompiledCall,
    pub limits: BTreeMap<String, u64>,
    pub records: BTreeMap<String, BTreeMap<String, CompiledType>>,
    pub tasks: BTreeMap<String, CompiledTask>,
    pub tool_bindings: Vec<CompiledToolBinding>,
}
