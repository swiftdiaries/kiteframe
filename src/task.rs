use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{GoalContract, GoalContractError, IdempotencyKey, SessionId, canonical_sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskTarget {
    GithubPullRequest { repository: String, number: u64 },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessRole {
    Reasoning,
    Coding,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_role: Option<HarnessRole>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentSelector {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmitUserTask {
    pub session_id: SessionId,
    pub agent: AgentSelector,
    pub target: TaskTarget,
    pub preferences: TaskPreferences,
    pub goal: GoalContract,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum UserTaskError {
    #[error(transparent)]
    InvalidGoal(#[from] GoalContractError),
    #[error("agent name must be a valid DSL identifier of at most 256 bytes")]
    InvalidAgentName,
    #[error("agent version must be greater than zero when supplied")]
    InvalidAgentVersion,
    #[error("GitHub repository must be an owner/name path of at most 255 characters")]
    InvalidRepository,
    #[error("GitHub pull request number must be greater than zero")]
    InvalidPullRequestNumber,
    #[error("task idempotency key must not be blank")]
    InvalidIdempotencyKey,
    #[error("task canonicalization failed: {0}")]
    Canonicalization(String),
}

fn valid_agent_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 256
        && matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_repository(value: &str) -> bool {
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    value.chars().count() <= 255
        && value.split('/').count() == 2
        && value.split('/').all(valid_segment)
}

impl SubmitUserTask {
    pub fn validate(&self) -> Result<(), UserTaskError> {
        self.goal.validate()?;
        if !valid_agent_name(&self.agent.name) {
            return Err(UserTaskError::InvalidAgentName);
        }
        if self.agent.version == Some(0) {
            return Err(UserTaskError::InvalidAgentVersion);
        }
        let TaskTarget::GithubPullRequest { repository, number } = &self.target;
        if !valid_repository(repository) {
            return Err(UserTaskError::InvalidRepository);
        }
        if *number == 0 {
            return Err(UserTaskError::InvalidPullRequestNumber);
        }
        if self.idempotency_key.as_str().trim().is_empty() {
            return Err(UserTaskError::InvalidIdempotencyKey);
        }
        Ok(())
    }

    pub fn request_digest(&self) -> Result<String, UserTaskError> {
        let mut value = serde_json::to_value(self)
            .map_err(|error| UserTaskError::Canonicalization(error.to_string()))?;
        value
            .as_object_mut()
            .expect("SubmitUserTask serializes as an object")
            .remove("idempotencyKey");
        canonical_sha256(&value).map_err(|error| UserTaskError::Canonicalization(error.to_string()))
    }
}
