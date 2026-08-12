mod transition;

pub use transition::{FrameSnapshot, TransitionError, next_command};

use std::sync::Arc;

use thiserror::Error;

use crate::{ModelClient, PostgresStore, SessionAuthorizer, StoreError, ToolResolver};

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
            let Some(outcome) = frame.outcome_name else { continue; };
            let bindings = frame.bindings.as_object().cloned().ok_or_else(|| StoreError::InvalidData("runnable frame bindings are malformed".to_owned()))?.into_iter().collect();
            let command = next_command(&frame.workflow, &FrameSnapshot { task_name: frame.task_name, outcome, statement_index: frame.statement_index, bindings })?;
            self.store.apply_frame_command(frame.id, frame.revision, command).await?;
            applied += 1;
        }
        Ok(applied)
    }
}
