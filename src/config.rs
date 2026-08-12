use crate::PrincipalId;

#[derive(Clone, Debug)]
pub enum HarnessMode { Stdio, Server { bind: std::net::SocketAddr } }

#[derive(Clone, Debug)]
pub struct HarnessConfig {
    pub database_url: String,
    pub mode: HarnessMode,
    pub bearer_token: Option<String>,
    pub principal: Option<PrincipalId>,
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = std::env::var("KITEFRAME_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL")).map_err(|_| ConfigError::MissingDatabaseUrl)?;
        let mode = match std::env::var("KITEFRAME_BIND") { Ok(bind) => HarnessMode::Server { bind: bind.parse().map_err(|_| ConfigError::InvalidBind(bind))? }, Err(_) => HarnessMode::Stdio };
        let bearer_token = std::env::var("KITEFRAME_BEARER_TOKEN").ok();
        let principal = std::env::var("KITEFRAME_PRINCIPAL").ok().map(PrincipalId::new).transpose().map_err(|error| ConfigError::InvalidPrincipal(error.to_string()))?;
        if bearer_token.is_some() != principal.is_some() { return Err(ConfigError::IncompleteBearerAuthentication); }
        if matches!(mode, HarnessMode::Server { .. }) && bearer_token.is_none() { return Err(ConfigError::MissingRemoteAuthentication); }
        Ok(Self { database_url, mode, bearer_token, principal })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("KITEFRAME_DATABASE_URL or DATABASE_URL must be configured")] MissingDatabaseUrl,
    #[error("invalid KITEFRAME_BIND: {0}")] InvalidBind(String),
    #[error("invalid configured principal: {0}")] InvalidPrincipal(String),
    #[error("KITEFRAME_BEARER_TOKEN and KITEFRAME_PRINCIPAL must be configured together")] IncompleteBearerAuthentication,
    #[error("remote server mode requires bearer authentication")] MissingRemoteAuthentication,
}
