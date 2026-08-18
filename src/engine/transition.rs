use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;

use crate::dsl::{CompiledDefinition, CompiledStatement, CompiledType, CompiledValue};
use crate::{Bindings, ClosedSchema, FrameCommand, NewChild, Question};

#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    pub task_name: String,
    pub outcome: String,
    pub statement_index: usize,
    pub bindings: Bindings,
}

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("compiled definition is corrupt: {0}")]
    Corrupt(String),
    #[error("binding was not present: {0}")]
    MissingBinding(String),
    #[error("unsupported nonterminal statement")]
    Unsupported,
}

pub fn next_command(
    definition: &CompiledDefinition,
    frame: &FrameSnapshot,
) -> Result<FrameCommand, TransitionError> {
    let task = definition
        .tasks
        .get(&frame.task_name)
        .ok_or_else(|| TransitionError::Corrupt("frame task is absent".to_owned()))?;
    let outcome = task
        .outcomes
        .get(&frame.outcome)
        .ok_or_else(|| TransitionError::Corrupt("frame outcome is absent".to_owned()))?;
    let statement = outcome
        .statements
        .get(frame.statement_index)
        .ok_or_else(|| TransitionError::Corrupt("frame cursor is absent".to_owned()))?;
    match statement {
        CompiledStatement::Goto { call } => Ok(FrameCommand::Goto {
            task_name: call.task.clone(),
            bindings: resolve_bindings(&call.arguments, &frame.bindings)?,
        }),
        CompiledStatement::Done { value } => {
            Ok(FrameCommand::Complete(resolve(value, &frame.bindings)?))
        }
        CompiledStatement::Fail { code, message } => Ok(FrameCommand::Fail {
            code: resolve(code, &frame.bindings)?
                .as_str()
                .ok_or_else(|| TransitionError::Corrupt("failure code is not a string".to_owned()))?
                .to_owned(),
            message: resolve(message, &frame.bindings)?
                .as_str()
                .ok_or_else(|| {
                    TransitionError::Corrupt("failure message is not a string".to_owned())
                })?
                .to_owned(),
        }),
        CompiledStatement::Call { binding, call } => Ok(FrameCommand::StartCall(NewChild {
            target: call.task.clone(),
            binding: binding.clone(),
            arguments: resolve_bindings(&call.arguments, &frame.bindings)?,
            branch_index: 0,
        })),
        CompiledStatement::Parallel { calls } => Ok(FrameCommand::StartParallel(
            calls
                .iter()
                .enumerate()
                .map(|(branch_index, child)| {
                    Ok(NewChild {
                        target: child.call.task.clone(),
                        binding: child.binding.clone(),
                        arguments: resolve_bindings(&child.call.arguments, &frame.bindings)?,
                        branch_index: branch_index as i32,
                    })
                })
                .collect::<Result<Vec<_>, TransitionError>>()?,
        )),
        CompiledStatement::Ask {
            binding,
            answer_type,
            question,
        } => Ok(FrameCommand::SuspendAsk(Question {
            id: uuid::Uuid::now_v7(),
            binding: binding.clone(),
            text: resolve(question, &frame.bindings)?
                .as_str()
                .ok_or_else(|| TransitionError::Corrupt("question is not a string".to_owned()))?
                .to_owned(),
            schema: closed_schema(definition, answer_type)?,
        })),
    }
}

fn closed_schema(
    definition: &CompiledDefinition,
    ty: &CompiledType,
) -> Result<ClosedSchema, TransitionError> {
    match ty {
        CompiledType::String => Ok(ClosedSchema::string()),
        CompiledType::Integer => Ok(ClosedSchema::integer()),
        CompiledType::Number => Ok(ClosedSchema::number()),
        CompiledType::Boolean => Ok(ClosedSchema::boolean()),
        CompiledType::Optional(inner) => {
            Ok(ClosedSchema::nullable(closed_schema(definition, inner)?))
        }
        CompiledType::List(inner) => Ok(ClosedSchema::array(closed_schema(definition, inner)?)),
        CompiledType::Record(name) => definition
            .records
            .get(name)
            .ok_or_else(|| TransitionError::Corrupt(format!("record type is absent: {name}")))?
            .iter()
            .map(|(name, ty)| Ok((name.clone(), closed_schema(definition, ty)?)))
            .collect::<Result<std::collections::BTreeMap<_, _>, TransitionError>>()
            .map(|properties| ClosedSchema::object("answer", properties)),
    }
}

fn resolve_bindings(
    source: &BTreeMap<String, CompiledValue>,
    bindings: &Bindings,
) -> Result<Bindings, TransitionError> {
    source
        .iter()
        .map(|(name, value)| Ok((name.clone(), resolve(value, bindings)?)))
        .collect()
}

fn resolve(value: &CompiledValue, bindings: &Bindings) -> Result<Value, TransitionError> {
    match value {
        CompiledValue::Binding(name) => bindings
            .get(name)
            .cloned()
            .ok_or_else(|| TransitionError::MissingBinding(name.clone())),
        CompiledValue::String(value) => Ok(Value::String(value.clone())),
        CompiledValue::Number(value) => serde_json::from_str(value)
            .map_err(|_| TransitionError::Corrupt("numeric literal is invalid".to_owned())),
        CompiledValue::Boolean(value) => Ok(Value::Bool(*value)),
        CompiledValue::Null => Ok(Value::Null),
    }
}
