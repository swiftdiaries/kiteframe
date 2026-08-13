use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

/// One stateless turn sent to the configured Responses-compatible provider.
#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub instructions: String,
    pub input: Vec<Value>,
    pub tools: Vec<Value>,
}

/// The provider's ordered output. Items remain raw so future requests can replay them exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    pub items: Vec<Value>,
}

impl ModelResponse {
    #[must_use]
    pub fn item_kind(item: &Value) -> ModelResponseItemKind {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => ModelResponseItemKind::Message,
            Some("refusal") => ModelResponseItemKind::Refusal,
            Some("reasoning") => ModelResponseItemKind::Reasoning,
            Some("function_call") => ModelResponseItemKind::FunctionCall,
            Some("function_call_output") => ModelResponseItemKind::FunctionOutput,
            _ => ModelResponseItemKind::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelResponseItemKind {
    Message,
    Refusal,
    Reasoning,
    FunctionCall,
    FunctionOutput,
    Unknown,
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelErrorKind {
    Configuration,
    RateLimited,
    Unavailable,
    Timeout,
    Protocol,
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("invalid Responses configuration: {0}")]
    Configuration(String),
    #[error("Responses provider rate limited the request")]
    RateLimited,
    #[error("Responses provider is unavailable: {0}")]
    Unavailable(String),
    #[error("Responses request timed out")]
    Timeout,
    #[error("invalid Responses response: {0}")]
    Protocol(String),
}

impl ModelError {
    #[must_use]
    pub const fn kind(&self) -> ModelErrorKind {
        match self {
            Self::Configuration(_) => ModelErrorKind::Configuration,
            Self::RateLimited => ModelErrorKind::RateLimited,
            Self::Unavailable(_) => ModelErrorKind::Unavailable,
            Self::Timeout => ModelErrorKind::Timeout,
            Self::Protocol(_) => ModelErrorKind::Protocol,
        }
    }
}
