use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical_sha256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalContract {
    pub version: u16,
    pub objective: String,
    pub success: Vec<SuccessCriterion>,
    pub constraints: Vec<String>,
    pub blocked_when: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuccessCriterion {
    pub id: String,
    pub statement: String,
    pub evidence: String,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum GoalContractError {
    #[error("unsupported goal contract version")]
    UnsupportedVersion,
    #[error("goal objective must be non-empty and at most 4096 characters")]
    InvalidObjective,
    #[error("goal must contain between 1 and 32 success criteria")]
    InvalidSuccessCount,
    #[error("goal success criterion id is invalid: {0}")]
    InvalidCriterionId(String),
    #[error("goal success criterion id is duplicated: {0}")]
    DuplicateCriterionId(String),
    #[error("goal success criterion statement must be non-empty and at most 2048 characters")]
    InvalidCriterionStatement,
    #[error("goal evidence type is invalid: {0}")]
    InvalidEvidenceType(String),
    #[error("goal constraints exceed the count or text limit")]
    InvalidConstraints,
    #[error("goal blocked conditions exceed the count or text limit")]
    InvalidBlockedConditions,
    #[error("goal canonicalization failed: {0}")]
    Canonicalization(String),
}

fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= maximum
}

fn lower_segment(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn criterion_id(value: &str) -> bool {
    value.len() <= 64 && lower_segment(value)
}

fn evidence_type(value: &str) -> bool {
    value.len() <= 128 && value.split('.').count() >= 2 && value.split('.').all(lower_segment)
}

impl GoalContract {
    pub fn validate(&self) -> Result<(), GoalContractError> {
        if self.version != 1 {
            return Err(GoalContractError::UnsupportedVersion);
        }
        if !bounded_text(&self.objective, 4_096) {
            return Err(GoalContractError::InvalidObjective);
        }
        if !(1..=32).contains(&self.success.len()) {
            return Err(GoalContractError::InvalidSuccessCount);
        }

        let mut ids = BTreeSet::new();
        for criterion in &self.success {
            if !criterion_id(&criterion.id) {
                return Err(GoalContractError::InvalidCriterionId(criterion.id.clone()));
            }
            if !ids.insert(criterion.id.clone()) {
                return Err(GoalContractError::DuplicateCriterionId(
                    criterion.id.clone(),
                ));
            }
            if !bounded_text(&criterion.statement, 2_048) {
                return Err(GoalContractError::InvalidCriterionStatement);
            }
            if !evidence_type(&criterion.evidence) {
                return Err(GoalContractError::InvalidEvidenceType(
                    criterion.evidence.clone(),
                ));
            }
        }

        if self.constraints.len() > 32
            || self
                .constraints
                .iter()
                .any(|value| !bounded_text(value, 2_048))
        {
            return Err(GoalContractError::InvalidConstraints);
        }
        if self.blocked_when.len() > 32
            || self
                .blocked_when
                .iter()
                .any(|value| !bounded_text(value, 2_048))
        {
            return Err(GoalContractError::InvalidBlockedConditions);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, GoalContractError> {
        let value = serde_json::to_value(self)
            .map_err(|error| GoalContractError::Canonicalization(error.to_string()))?;
        canonical_sha256(&value)
            .map_err(|error| GoalContractError::Canonicalization(error.to_string()))
    }
}
