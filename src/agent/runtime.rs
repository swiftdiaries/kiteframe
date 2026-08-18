use std::collections::BTreeSet;

use serde_json::json;
use thiserror::Error;

use crate::dsl::CompiledDefinition;
use crate::{AgentExecutionView, canonical_json, canonical_sha256};

use super::{
    KernelCommand, KernelCommandPayload, RuntimeCeilings, RuntimeDisposition, RuntimeEvent,
    RuntimeStateSnapshot, RuntimeTransition,
};

pub struct RuntimeStart<'a> {
    pub execution: &'a AgentExecutionView,
    pub definition: &'a CompiledDefinition,
}

pub struct RuntimeHandle<'a> {
    pub execution: &'a AgentExecutionView,
    pub definition: &'a CompiledDefinition,
    pub event: &'a RuntimeEvent,
}

pub trait AgentRuntime: Send + Sync {
    fn validate_agent(&self, definition: &CompiledDefinition) -> Result<(), AgentRuntimeError>;
    fn start(&self, input: RuntimeStart<'_>) -> Result<RuntimeTransition, AgentRuntimeError>;
    fn handle(&self, input: RuntimeHandle<'_>) -> Result<RuntimeTransition, AgentRuntimeError>;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AgentRuntimeError {
    #[error("agent definition is invalid: {0}")]
    InvalidDefinition(String),
    #[error("runtime state is invalid: {0}")]
    InvalidState(String),
    #[error("runtime transition is invalid: {0}")]
    InvalidTransition(String),
    #[error("runtime value is invalid: {0}")]
    InvalidValue(String),
    #[error("runtime ceiling exceeded: {0}")]
    CeilingExceeded(&'static str),
    #[error("runtime canonicalization failed: {0}")]
    Canonicalization(String),
}

impl RuntimeStateSnapshot {
    pub fn new(
        disposition: RuntimeDisposition,
        payload: serde_json::Value,
        ceilings: &RuntimeCeilings,
    ) -> Result<Self, AgentRuntimeError> {
        ceilings.validate()?;
        let normalized_payload: serde_json::Value = serde_json::from_slice(
            &canonical_json(&payload)
                .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?,
        )
        .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?;
        let content = json!({
            "schemaVersion": 1,
            "runtimeId": "kite.v1",
            "disposition": disposition,
            "payload": normalized_payload,
        });
        let bytes = canonical_json(&content)
            .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?;
        if bytes.len() > ceilings.max_snapshot_bytes {
            return Err(AgentRuntimeError::CeilingExceeded("snapshot_bytes"));
        }
        Ok(Self {
            schema_version: 1,
            runtime_id: "kite.v1".into(),
            disposition,
            payload: content["payload"].clone(),
            ceiling_bytes: u32::try_from(ceilings.max_snapshot_bytes)
                .map_err(|_| AgentRuntimeError::CeilingExceeded("snapshot_bytes"))?,
            canonical_byte_count: u32::try_from(bytes.len())
                .map_err(|_| AgentRuntimeError::CeilingExceeded("snapshot_bytes"))?,
            snapshot_digest: canonical_sha256(&content)
                .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?,
        })
    }

    pub fn canonical_bytes(
        &self,
        ceilings: &RuntimeCeilings,
    ) -> Result<Vec<u8>, AgentRuntimeError> {
        ceilings.validate()?;
        if self.schema_version != 1 || self.runtime_id != "kite.v1" {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot envelope is unsupported".into(),
            ));
        }
        if !self.payload.is_object() {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot payload must be an object".into(),
            ));
        }
        if self.ceiling_bytes == 0 || self.ceiling_bytes > 262_144 {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot ceiling is invalid".into(),
            ));
        }
        let content = json!({
            "schemaVersion": self.schema_version,
            "runtimeId": &self.runtime_id,
            "disposition": &self.disposition,
            "payload": &self.payload,
        });
        let bytes = canonical_json(&content)
            .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?;
        if bytes.len() > ceilings.max_snapshot_bytes || bytes.len() > self.ceiling_bytes as usize {
            return Err(AgentRuntimeError::CeilingExceeded("snapshot_bytes"));
        }
        if bytes.len() != self.canonical_byte_count as usize
            || canonical_sha256(&content)
                .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?
                != self.snapshot_digest
        {
            return Err(AgentRuntimeError::InvalidState(
                "snapshot integrity metadata does not match".into(),
            ));
        }
        Ok(bytes)
    }

    pub fn digest(&self, ceilings: &RuntimeCeilings) -> Result<String, AgentRuntimeError> {
        self.canonical_bytes(ceilings)?;
        Ok(self.snapshot_digest.clone())
    }
}

pub fn validate_transition(
    transition: &RuntimeTransition,
    execution: &AgentExecutionView,
    ceilings: &RuntimeCeilings,
) -> Result<(), AgentRuntimeError> {
    ceilings.validate()?;
    if transition.expected_task_revision != execution.task_revision {
        return Err(AgentRuntimeError::InvalidTransition(
            "task revision does not match".into(),
        ));
    }
    if transition.expected_goal_revision != execution.goal_revision {
        return Err(AgentRuntimeError::InvalidTransition(
            "goal revision does not match".into(),
        ));
    }
    if !(1..=128).contains(&transition.idempotency_key.len()) {
        return Err(AgentRuntimeError::InvalidTransition(
            "idempotency key is invalid".into(),
        ));
    }
    if transition.commands.len() > ceilings.max_commands_per_transition {
        return Err(AgentRuntimeError::CeilingExceeded(
            "commands_per_transition",
        ));
    }

    transition.snapshot.canonical_bytes(ceilings)?;

    let mut keys = BTreeSet::new();
    for command in &transition.commands {
        if command.schema_version != 1 {
            return Err(AgentRuntimeError::InvalidTransition(
                "command schema version is unsupported".into(),
            ));
        }
        super::RuntimeCommandKey::new(command.key.as_str())?;
        if !keys.insert(command.key.as_str()) {
            return Err(AgentRuntimeError::InvalidTransition(
                "command key is duplicated".into(),
            ));
        }
        let bytes = canonical_json(command)
            .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?;
        if bytes.len() > ceilings.max_command_bytes {
            return Err(AgentRuntimeError::CeilingExceeded("command_bytes"));
        }
    }

    match transition.snapshot.disposition {
        RuntimeDisposition::Running if transition.commands.is_empty() => {
            return Err(AgentRuntimeError::InvalidTransition(
                "running snapshot requires a command".into(),
            ));
        }
        RuntimeDisposition::Returned if !transition.commands.is_empty() => {
            return Err(AgentRuntimeError::InvalidTransition(
                "returned snapshot cannot emit commands".into(),
            ));
        }
        RuntimeDisposition::Blocked => {
            if !matches!(
                transition.commands.as_slice(),
                [KernelCommand {
                    payload: KernelCommandPayload::BlockGoal(_),
                    ..
                }]
            ) {
                return Err(AgentRuntimeError::InvalidTransition(
                    "blocked snapshot requires one BlockGoal".into(),
                ));
            }
        }
        RuntimeDisposition::Terminating => {
            if !matches!(
                transition.commands.as_slice(),
                [KernelCommand {
                    payload: KernelCommandPayload::CompleteGoal(_)
                        | KernelCommandPayload::FailTask(_),
                    ..
                }]
            ) {
                return Err(AgentRuntimeError::InvalidTransition(
                    "terminating snapshot requires one authority command".into(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}
