use std::{collections::HashSet, fmt, time::Duration};

use crate::{PrincipalId, ResponsesConfig, Secret};

#[derive(Clone, Debug)]
pub enum HarnessMode {
    Stdio,
    Server { bind: std::net::SocketAddr },
}

#[derive(Clone)]
pub struct BearerBinding {
    pub secret: Secret,
    pub principal: PrincipalId,
}

impl BearerBinding {
    #[must_use]
    pub fn new(secret: Secret, principal: PrincipalId) -> Self {
        Self { secret, principal }
    }
}

impl fmt::Debug for BearerBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BearerBinding")
            .field("secret", &"[REDACTED]")
            .field("principal", &self.principal)
            .finish()
    }
}

#[derive(Clone)]
pub struct HarnessConfig {
    pub database_url: String,
    pub mode: HarnessMode,
    pub local_principal: Option<PrincipalId>,
    pub bearer_bindings: Vec<BearerBinding>,
    pub model: ResponsesConfig,
    pub acp_v2_enabled: bool,
    pub max_remote_connections: usize,
}

impl fmt::Debug for HarnessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HarnessConfig")
            .field("database_url", &"[REDACTED]")
            .field("mode", &self.mode)
            .field("local_principal", &self.local_principal)
            .field("bearer_bindings", &self.bearer_bindings)
            .field("model", &self.model)
            .field("acp_v2_enabled", &self.acp_v2_enabled)
            .field("max_remote_connections", &self.max_remote_connections)
            .finish()
    }
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_values(|name| std::env::var(name).ok())
    }

    fn from_values(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let database_url = value("KITEFRAME_DATABASE_URL")
            .or_else(|| value("DATABASE_URL"))
            .ok_or(ConfigError::MissingDatabaseUrl)?;
        let mode = match value("KITEFRAME_BIND") {
            Some(bind) => HarnessMode::Server {
                bind: bind.parse().map_err(|_| ConfigError::InvalidBind(bind))?,
            },
            None => HarnessMode::Stdio,
        };
        let configured_principal = value("KITEFRAME_PRINCIPAL")
            .map(PrincipalId::new)
            .transpose()
            .map_err(|error| ConfigError::InvalidPrincipal(error.to_string()))?;
        let legacy_token = value("KITEFRAME_BEARER_TOKEN");
        if legacy_token.is_some() != configured_principal.is_some()
            && value("KITEFRAME_BEARER_BINDINGS").is_none()
            && matches!(mode, HarnessMode::Server { .. })
        {
            return Err(ConfigError::IncompleteBearerAuthentication);
        }

        let bearer_bindings = match value("KITEFRAME_BEARER_BINDINGS") {
            Some(bindings) => parse_bindings(&bindings)?,
            None => match (legacy_token, configured_principal.clone()) {
                (Some(secret), Some(principal)) => {
                    validate_secret(&secret)?;
                    vec![BearerBinding::new(Secret::new(secret), principal)]
                }
                _ => Vec::new(),
            },
        };

        let local_principal = match mode {
            HarnessMode::Stdio => {
                Some(configured_principal.ok_or(ConfigError::MissingLocalPrincipal)?)
            }
            HarnessMode::Server { .. } => {
                if bearer_bindings.is_empty() {
                    return Err(ConfigError::MissingRemoteAuthentication);
                }
                if !parse_bool(value("KITEFRAME_TLS_TERMINATED").as_deref())? {
                    return Err(ConfigError::TlsTerminationRequired);
                }
                None
            }
        };

        let base_url =
            value("KITEFRAME_RESPONSES_BASE_URL").ok_or(ConfigError::MissingModelConfiguration)?;
        let api_key =
            value("KITEFRAME_RESPONSES_API_KEY").ok_or(ConfigError::MissingModelConfiguration)?;
        let model_name =
            value("KITEFRAME_RESPONSES_MODEL").ok_or(ConfigError::MissingModelConfiguration)?;
        let model = ResponsesConfig::new(
            base_url,
            Secret::new(api_key),
            model_name,
            Duration::from_secs(60),
            1024 * 1024,
        );
        let acp_v2_enabled = parse_bool(value("KITEFRAME_ENABLE_ACP_V2").as_deref())?;
        if acp_v2_enabled && !cfg!(feature = "acp-v2") {
            return Err(ConfigError::AcpV2Unavailable);
        }
        let max_remote_connections = value("KITEFRAME_MAX_REMOTE_CONNECTIONS")
            .map(|value| value.parse::<usize>())
            .transpose()
            .map_err(|_| ConfigError::InvalidRemoteConnectionLimit)?
            .unwrap_or(64);
        if !(1..=4096).contains(&max_remote_connections) {
            return Err(ConfigError::InvalidRemoteConnectionLimit);
        }

        Ok(Self {
            database_url,
            mode,
            local_principal,
            bearer_bindings,
            model,
            acp_v2_enabled,
            max_remote_connections,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedBinding {
    secret: String,
    principal: String,
}

fn parse_bindings(value: &str) -> Result<Vec<BearerBinding>, ConfigError> {
    let serialized: Vec<SerializedBinding> =
        serde_json::from_str(value).map_err(|_| ConfigError::InvalidBearerBindings)?;
    if serialized.is_empty() {
        return Err(ConfigError::MissingRemoteAuthentication);
    }
    let mut secrets = HashSet::new();
    serialized
        .into_iter()
        .map(|binding| {
            validate_secret(&binding.secret)?;
            if !secrets.insert(binding.secret.clone()) {
                return Err(ConfigError::DuplicateBearerSecret);
            }
            let principal = PrincipalId::new(binding.principal)
                .map_err(|error| ConfigError::InvalidPrincipal(error.to_string()))?;
            Ok(BearerBinding::new(Secret::new(binding.secret), principal))
        })
        .collect()
}

fn validate_secret(secret: &str) -> Result<(), ConfigError> {
    if secret.is_empty() {
        Err(ConfigError::EmptyBearerSecret)
    } else {
        Ok(())
    }
}

fn parse_bool(value: Option<&str>) -> Result<bool, ConfigError> {
    match value {
        None | Some("0" | "false" | "FALSE") => Ok(false),
        Some("1" | "true" | "TRUE") => Ok(true),
        Some(_) => Err(ConfigError::InvalidBoolean),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("KITEFRAME_DATABASE_URL or DATABASE_URL must be configured")]
    MissingDatabaseUrl,
    #[error("invalid KITEFRAME_BIND: {0}")]
    InvalidBind(String),
    #[error("invalid configured principal: {0}")]
    InvalidPrincipal(String),
    #[error("KITEFRAME_PRINCIPAL must be configured for stdio mode")]
    MissingLocalPrincipal,
    #[error("KITEFRAME_BEARER_TOKEN and KITEFRAME_PRINCIPAL must be configured together")]
    IncompleteBearerAuthentication,
    #[error("remote server mode requires one or more bearer bindings")]
    MissingRemoteAuthentication,
    #[error("KITEFRAME_BEARER_BINDINGS must be a JSON array of secret/principal objects")]
    InvalidBearerBindings,
    #[error("bearer secrets must be non-empty")]
    EmptyBearerSecret,
    #[error("bearer secrets must be unique")]
    DuplicateBearerSecret,
    #[error("remote mode requires KITEFRAME_TLS_TERMINATED=true")]
    TlsTerminationRequired,
    #[error("configuration boolean must be true, false, 1, or 0")]
    InvalidBoolean,
    #[error(
        "KITEFRAME_RESPONSES_BASE_URL, KITEFRAME_RESPONSES_API_KEY, and KITEFRAME_RESPONSES_MODEL must be configured"
    )]
    MissingModelConfiguration,
    #[error("ACP v2 was requested but the binary was not built with the acp-v2 feature")]
    AcpV2Unavailable,
    #[error("KITEFRAME_MAX_REMOTE_CONNECTIONS must be between 1 and 4096")]
    InvalidRemoteConnectionLimit,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{ConfigError, HarnessConfig, HarnessMode};

    fn config(values: &[(&str, &str)]) -> Result<HarnessConfig, ConfigError> {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        HarnessConfig::from_values(|name| values.get(name).cloned())
    }

    fn model_values() -> [(&'static str, &'static str); 3] {
        [
            ("KITEFRAME_RESPONSES_BASE_URL", "https://model.example/v1"),
            ("KITEFRAME_RESPONSES_API_KEY", "model-sentinel-secret"),
            ("KITEFRAME_RESPONSES_MODEL", "test-model"),
        ]
    }

    #[test]
    fn stdio_requires_a_local_principal_and_model_configuration() {
        let mut values = vec![("DATABASE_URL", "postgres://db-secret@localhost/kiteframe")];
        values.extend(model_values());
        assert!(matches!(
            config(&values),
            Err(ConfigError::MissingLocalPrincipal)
        ));
        values.push(("KITEFRAME_PRINCIPAL", "local-user"));
        let configured = config(&values).expect("stdio config");
        assert!(matches!(configured.mode, HarnessMode::Stdio));
        assert_eq!(configured.local_principal.unwrap().as_str(), "local-user");
    }

    #[test]
    fn remote_requires_tls_and_accepts_a_binding_set_without_a_body_principal() {
        let bindings = r#"[{"secret":"first-secret","principal":"alice"},{"secret":"second-secret","principal":"bob"}]"#;
        let mut values = vec![
            ("DATABASE_URL", "postgres://localhost/kiteframe"),
            ("KITEFRAME_BIND", "127.0.0.1:0"),
            ("KITEFRAME_BEARER_BINDINGS", bindings),
        ];
        values.extend(model_values());
        assert!(matches!(
            config(&values),
            Err(ConfigError::TlsTerminationRequired)
        ));
        values.push(("KITEFRAME_TLS_TERMINATED", "true"));
        let configured = config(&values).expect("remote config");
        assert!(configured.local_principal.is_none());
        assert_eq!(configured.bearer_bindings.len(), 2);
    }

    #[test]
    fn debug_output_redacts_database_model_and_bearer_secrets() {
        let bindings = r#"[{"secret":"bearer-sentinel-secret","principal":"alice"}]"#;
        let mut values = vec![
            (
                "DATABASE_URL",
                "postgres://database-sentinel-secret@localhost/kiteframe",
            ),
            ("KITEFRAME_BIND", "127.0.0.1:0"),
            ("KITEFRAME_BEARER_BINDINGS", bindings),
            ("KITEFRAME_TLS_TERMINATED", "true"),
        ];
        values.extend(model_values());
        let debug = format!("{:?}", config(&values).expect("config"));
        for secret in [
            "database-sentinel-secret",
            "model-sentinel-secret",
            "bearer-sentinel-secret",
        ] {
            assert!(!debug.contains(secret), "debug output leaked {secret}");
        }
    }

    #[test]
    fn remote_connection_limit_is_bounded_and_configurable() {
        let bindings = r#"[{"secret":"secret","principal":"alice"}]"#;
        let mut values = vec![
            ("DATABASE_URL", "postgres://localhost/kiteframe"),
            ("KITEFRAME_BIND", "127.0.0.1:0"),
            ("KITEFRAME_BEARER_BINDINGS", bindings),
            ("KITEFRAME_TLS_TERMINATED", "true"),
            ("KITEFRAME_MAX_REMOTE_CONNECTIONS", "7"),
        ];
        values.extend(model_values());
        assert_eq!(config(&values).unwrap().max_remote_connections, 7);
        for invalid in ["0", "4097", "not-a-number"] {
            let mut invalid_values = values.clone();
            invalid_values.retain(|(key, _)| *key != "KITEFRAME_MAX_REMOTE_CONNECTIONS");
            invalid_values.push(("KITEFRAME_MAX_REMOTE_CONNECTIONS", invalid));
            assert!(matches!(
                config(&invalid_values),
                Err(ConfigError::InvalidRemoteConnectionLimit)
            ));
        }
    }
}
