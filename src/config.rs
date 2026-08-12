use std::time::Duration;

use crate::{PrincipalId, ResponsesConfig, Secret};

#[derive(Clone, Debug)]
pub enum HarnessMode { Stdio, Server { bind: std::net::SocketAddr } }

#[derive(Clone, Debug)]
pub struct HarnessConfig {
    pub database_url: String,
    pub mode: HarnessMode,
    pub bearer_token: Option<String>,
    pub principal: PrincipalId,
    pub model: ResponsesConfig,
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = std::env::var("KITEFRAME_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL")).map_err(|_| ConfigError::MissingDatabaseUrl)?;
        let mode = match std::env::var("KITEFRAME_BIND") { Ok(bind) => HarnessMode::Server { bind: bind.parse().map_err(|_| ConfigError::InvalidBind(bind))? }, Err(_) => HarnessMode::Stdio };
        let bearer_token = std::env::var("KITEFRAME_BEARER_TOKEN").ok();
        let principal = PrincipalId::new(std::env::var("KITEFRAME_PRINCIPAL").map_err(|_| ConfigError::MissingPrincipal)?)
            .map_err(|error| ConfigError::InvalidPrincipal(error.to_string()))?;
        if matches!(mode, HarnessMode::Server { .. }) && bearer_token.is_none() { return Err(ConfigError::MissingRemoteAuthentication); }
        let base_url = std::env::var("KITEFRAME_RESPONSES_BASE_URL").map_err(|_| ConfigError::MissingModelConfiguration)?;
        let api_key = std::env::var("KITEFRAME_RESPONSES_API_KEY").map_err(|_| ConfigError::MissingModelConfiguration)?;
        let model = std::env::var("KITEFRAME_RESPONSES_MODEL").map_err(|_| ConfigError::MissingModelConfiguration)?;
        let model = ResponsesConfig::new(base_url, Secret::new(api_key), model, Duration::from_secs(60), 1024 * 1024);
        Ok(Self { database_url, mode, bearer_token, principal, model })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("KITEFRAME_DATABASE_URL or DATABASE_URL must be configured")] MissingDatabaseUrl,
    #[error("invalid KITEFRAME_BIND: {0}")] InvalidBind(String),
    #[error("invalid configured principal: {0}")] InvalidPrincipal(String),
    #[error("KITEFRAME_PRINCIPAL must be configured")] MissingPrincipal,
    #[error("KITEFRAME_RESPONSES_BASE_URL, KITEFRAME_RESPONSES_API_KEY, and KITEFRAME_RESPONSES_MODEL must be configured")] MissingModelConfiguration,
    #[error("remote server mode requires bearer authentication")] MissingRemoteAuthentication,
}
