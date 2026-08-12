use std::collections::BTreeMap;

use serde_json::{Number, Value};
use thiserror::Error;

use crate::dsl::{CompiledStatement, CompiledValue, CompiledWorkflow};
use crate::{Bindings, FrameCommand};

#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    pub task_name: String,
    pub outcome: String,
    pub statement_index: usize,
    pub bindings: Bindings,
}

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("compiled workflow is corrupt: {0}")]
    Corrupt(String),
    #[error("binding was not present: {0}")]
    MissingBinding(String),
    #[error("unsupported nonterminal statement")]
    Unsupported,
}

pub fn next_command(
    workflow: &CompiledWorkflow,
    frame: &FrameSnapshot,
) -> Result<FrameCommand, TransitionError> {
    let task = workflow.tasks.get(&frame.task_name).ok_or_else(|| TransitionError::Corrupt("frame task is absent".to_owned()))?;
    let outcome = task.outcomes.get(&frame.outcome).ok_or_else(|| TransitionError::Corrupt("frame outcome is absent".to_owned()))?;
    let statement = outcome.statements.get(frame.statement_index).ok_or_else(|| TransitionError::Corrupt("frame cursor is absent".to_owned()))?;
    match statement {
        CompiledStatement::Goto { call } => Ok(FrameCommand::Goto {
            task_name: call.task.clone(),
            bindings: resolve_bindings(&call.arguments, &frame.bindings)?,
        }),
        CompiledStatement::Done { value } => Ok(FrameCommand::Complete(resolve(value, &frame.bindings)?)),
        CompiledStatement::Fail { code, message } => Ok(FrameCommand::Fail {
            code: resolve(code, &frame.bindings)?.as_str().ok_or_else(|| TransitionError::Corrupt("failure code is not a string".to_owned()))?.to_owned(),
            message: resolve(message, &frame.bindings)?.as_str().ok_or_else(|| TransitionError::Corrupt("failure message is not a string".to_owned()))?.to_owned(),
        }),
        CompiledStatement::Call { .. } | CompiledStatement::Parallel { .. } | CompiledStatement::Ask { .. } => Err(TransitionError::Unsupported),
    }
}

fn resolve_bindings(source: &BTreeMap<String, CompiledValue>, bindings: &Bindings) -> Result<Bindings, TransitionError> {
    source.iter().map(|(name, value)| Ok((name.clone(), resolve(value, bindings)?))).collect()
}

fn resolve(value: &CompiledValue, bindings: &Bindings) -> Result<Value, TransitionError> {
    match value {
        CompiledValue::Binding(name) => bindings.get(name).cloned().ok_or_else(|| TransitionError::MissingBinding(name.clone())),
        CompiledValue::String(value) => Ok(Value::String(value.clone())),
        CompiledValue::Number(value) => value.parse::<f64>().ok().and_then(Number::from_f64).map(Value::Number).ok_or_else(|| TransitionError::Corrupt("numeric literal is invalid".to_owned())),
        CompiledValue::Boolean(value) => Ok(Value::Bool(*value)),
        CompiledValue::Null => Ok(Value::Null),
    }
}
