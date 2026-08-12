mod transition;

pub use transition::{FrameSnapshot, TransitionError, next_command};

use std::sync::Arc;

use thiserror::Error;
use serde_json::{Value, json};

use crate::{ModelClient, ModelRequest, PostgresStore, SessionAuthorizer, StoreError, ToolResolver};

pub struct Engine {
    pub store: Arc<PostgresStore>,
    pub model: Arc<dyn ModelClient>,
    pub tools: Arc<dyn ToolResolver>,
    pub authorizer: Arc<dyn SessionAuthorizer>,
    pub config: EngineConfig,
}

#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub scheduler_batch_size: i64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { scheduler_batch_size: 32 }
    }
}

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
}

impl Engine {
    pub async fn tick_until_idle(&self) -> Result<usize, EngineError> {
        let frames = self.store.runnable_frames(self.config.scheduler_batch_size).await?;
        let mut applied = 0;
        for frame in frames {
            let Some(outcome) = frame.outcome_name else {
                self.run_model_turn(frame).await?;
                applied += 1;
                continue;
            };
            let bindings = frame.bindings.as_object().cloned().ok_or_else(|| StoreError::InvalidData("runnable frame bindings are malformed".to_owned()))?.into_iter().collect();
            let command = next_command(&frame.workflow, &FrameSnapshot { task_name: frame.task_name, outcome, statement_index: frame.statement_index, bindings })?;
            self.store.apply_frame_command(frame.id, frame.revision, command).await?;
            applied += 1;
        }
        Ok(applied)
    }

    async fn run_model_turn(&self, frame: crate::RunnableFrame) -> Result<(), EngineError> {
        let task = frame.workflow.tasks.get(&frame.task_name).ok_or_else(|| StoreError::InvalidData("runnable frame task is absent from workflow".to_owned()))?;
        let attempt = self.store.begin_model_attempt(frame.id.clone(), frame.revision, json!({"bindings": frame.bindings})).await?;
        let response = self.model.respond(ModelRequest {
            instructions: task.prompt.clone(),
            input: vec![json!({"bindings": frame.bindings})],
            tools: vec![completion_tool(task)],
        }).await.map_err(|error| StoreError::InvalidData(error.to_string()))?;
        self.store.commit_model_attempt(frame.id.clone(), attempt.attempt, attempt.revision, response.items.clone()).await?;
        let completion = response.items.iter().filter_map(completion_from_item).next_back().ok_or_else(|| StoreError::InvalidData("model response did not contain complete_task".to_owned()))?;
        self.store.record_completion(frame.id, attempt.revision + 1, completion.0, completion.1).await?;
        Ok(())
    }
}

fn completion_tool(task: &crate::dsl::CompiledTask) -> Value {
    json!({"type":"function", "name":"complete_task", "description":"Complete the current task with one declared outcome.", "parameters":{"type":"object", "additionalProperties":false, "properties":{"outcome":{"type":"string"}, "fields":{"type":"object"}}, "required":["outcome", "fields"]}, "outcomes": task.completion.outcomes})
}

fn completion_from_item(item: &Value) -> Option<(String, Value)> {
    if item.get("type")?.as_str()? != "function_call" || item.get("name")?.as_str()? != "complete_task" { return None; }
    let arguments = item.get("arguments")?;
    let arguments = arguments.as_str().and_then(|text| serde_json::from_str(text).ok()).unwrap_or_else(|| arguments.clone());
    Some((arguments.get("outcome")?.as_str()?.to_owned(), arguments.get("fields")?.clone()))
}
