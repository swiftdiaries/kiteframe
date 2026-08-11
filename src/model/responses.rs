use std::time::Duration;

use async_trait::async_trait;
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

use crate::Secret;

use super::{ModelClient, ModelError, ModelRequest, ModelResponse};

/// Deployment-owned connection settings for the portable Responses profile.
#[derive(Clone, Debug)]
pub struct ResponsesConfig {
    base_url: String,
    api_key: Secret,
    model: String,
    timeout: Duration,
    max_response_item_bytes: usize,
}

impl ResponsesConfig {
    pub fn new(
        base_url: impl AsRef<str>,
        api_key: Secret,
        model: impl Into<String>,
        timeout: Duration,
        max_response_item_bytes: usize,
    ) -> Self {
        Self {
            base_url: base_url.as_ref().to_owned(),
            api_key,
            model: model.into(),
            timeout,
            max_response_item_bytes,
        }
    }

    fn validate(&self) -> Result<(), ModelError> {
        let base_url = Url::parse(&self.base_url)
            .map_err(|error| ModelError::Configuration(format!("invalid base URL: {error}")))?;
        if base_url.scheme() != "http" && base_url.scheme() != "https" {
            return Err(ModelError::Configuration(
                "base URL must use HTTP or HTTPS".to_owned(),
            ));
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            return Err(ModelError::Configuration(
                "base URL must not contain userinfo".to_owned(),
            ));
        }
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(ModelError::Configuration(
                "base URL must not contain a query or fragment".to_owned(),
            ));
        }
        if base_url.path().is_empty() || base_url.path() == "/" {
            return Err(ModelError::Configuration(
                "base URL must end at an API prefix".to_owned(),
            ));
        }
        if self.model.trim().is_empty() {
            return Err(ModelError::Configuration(
                "model must not be empty".to_owned(),
            ));
        }
        if self.timeout.is_zero() {
            return Err(ModelError::Configuration(
                "timeout must be positive".to_owned(),
            ));
        }
        if self.max_response_item_bytes == 0 {
            return Err(ModelError::Configuration(
                "response item limit must be positive".to_owned(),
            ));
        }
        Ok(())
    }

    fn responses_url(&self) -> Result<Url, ModelError> {
        let mut url = Url::parse(&self.base_url)
            .map_err(|error| ModelError::Configuration(format!("invalid base URL: {error}")))?;
        let path = format!("{}/responses", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(url)
    }
}

#[derive(Debug)]
pub struct ResponsesClient {
    config: ResponsesConfig,
    client: reqwest::Client,
}

impl ResponsesClient {
    pub fn new(config: ResponsesConfig) -> Result<Self, ModelError> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|error| {
                ModelError::Configuration(format!("cannot create HTTP client: {error}"))
            })?;
        Ok(Self { config, client })
    }

    pub async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        <Self as ModelClient>::respond(self, request).await
    }

    fn request_body(&self, request: ModelRequest) -> Value {
        json!({
            "model": self.config.model,
            "instructions": request.instructions,
            "input": request.input,
            "tools": request.tools,
            "tool_choice": "auto",
            "store": false,
        })
    }

    fn parse_response(&self, response: Value) -> Result<ModelResponse, ModelError> {
        if response.get("status").and_then(Value::as_str) == Some("incomplete") {
            return Err(ModelError::Protocol("response is incomplete".to_owned()));
        }
        let output = response
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| ModelError::Protocol("missing array output".to_owned()))?;
        if output.is_empty() {
            return Err(ModelError::Protocol("output must not be empty".to_owned()));
        }

        let mut call_ids = std::collections::BTreeSet::new();
        for item in output {
            let item_size = serde_json::to_vec(item)
                .map_err(|error| {
                    ModelError::Protocol(format!("cannot encode output item: {error}"))
                })?
                .len();
            if item_size > self.config.max_response_item_bytes {
                return Err(ModelError::Protocol(
                    "output item exceeds configured byte limit".to_owned(),
                ));
            }
            validate_known_item(item, &mut call_ids)?;
        }
        Ok(ModelResponse {
            items: output.clone(),
        })
    }
}

#[async_trait]
impl ModelClient for ResponsesClient {
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let response = self
            .client
            .post(self.config.responses_url()?)
            .bearer_auth(self.config.api_key.expose())
            .json(&self.request_body(request))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ModelError::Timeout
                } else {
                    ModelError::Unavailable(error.to_string())
                }
            })?;

        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(ModelError::RateLimited);
        }
        if status.is_server_error() {
            return Err(ModelError::Unavailable(format!(
                "provider returned {status}"
            )));
        }
        if !status.is_success() {
            return Err(ModelError::Protocol(format!("provider returned {status}")));
        }
        let body = response
            .json::<Value>()
            .await
            .map_err(|error| ModelError::Protocol(format!("response is not JSON: {error}")))?;
        self.parse_response(body)
    }
}

fn validate_known_item(
    item: &Value,
    call_ids: &mut std::collections::BTreeSet<String>,
) -> Result<(), ModelError> {
    if !item.is_object() {
        return Err(ModelError::Protocol(
            "output item must be an object".to_owned(),
        ));
    }
    let Some(kind) = item.get("type").and_then(Value::as_str) else {
        return Err(ModelError::Protocol(
            "output item is missing string type".to_owned(),
        ));
    };
    if kind != "function_call" {
        return Ok(());
    }
    let call_id = required_string(item, "call_id")?;
    required_string(item, "name")?;
    let arguments = required_string(item, "arguments")?;
    serde_json::from_str::<Value>(arguments)
        .map_err(|_| ModelError::Protocol("function_call arguments must be JSON".to_owned()))?;
    if !call_ids.insert(call_id.to_owned()) {
        return Err(ModelError::Protocol(format!(
            "duplicate function call id: {call_id}"
        )));
    }
    Ok(())
}

fn required_string<'a>(item: &'a Value, field: &str) -> Result<&'a str, ModelError> {
    item.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ModelError::Protocol(format!("function_call is missing string {field}")))
}
