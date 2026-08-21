use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fmt,
    path::Path,
    time::Duration,
};

use crate::{
    PrincipalId, ResponsesConfig, RuntimeCeilings, Secret, StoreLimits,
    harness::{AcpCommand, HarnessProfileId, HarnessRouteTable},
};

const BUILTIN_HARNESS_PROFILE_ID: &str = "builtin-v1";
const CODEX_ACP_COMMAND: &str = "KITEFRAME_CODEX_ACP_COMMAND_JSON";
const CODEX_ACP_SOURCE_REVISION: &str = "KITEFRAME_CODEX_ACP_SOURCE_REVISION";
const DEEPSEEK_ACP_COMMAND: &str = "KITEFRAME_DSH_ACP_COMMAND_JSON";
const DEEPSEEK_ACP_SOURCE_REVISION: &str = "KITEFRAME_DSH_ACP_SOURCE_REVISION";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExternalHarnessCommands {
    pub codex: Option<AcpCommand>,
    pub deepseek: Option<AcpCommand>,
}

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
    pub store_limits: StoreLimits,
    external_harness_commands: ExternalHarnessCommands,
    harness_routes: HarnessRouteTable,
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
            .field("store_limits", &self.store_limits)
            .field("external_harness_commands", &"[REDACTED]")
            .field("harness_routes", &self.harness_routes)
            .finish()
    }
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_values(|name| std::env::var(name).ok())
    }

    /// Builds a complete configuration with deterministic non-secret defaults for integration
    /// tests that only exercise deployment configuration parsing.
    pub fn from_test_values(
        values: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> Result<Self, ConfigError> {
        let mut configured = HashMap::from([
            (
                "KITEFRAME_DATABASE_URL".to_owned(),
                "postgres://test".to_owned(),
            ),
            (
                "KITEFRAME_PRINCIPAL".to_owned(),
                "test-principal".to_owned(),
            ),
            (
                "KITEFRAME_RESPONSES_BASE_URL".to_owned(),
                "https://model.example/v1".to_owned(),
            ),
            (
                "KITEFRAME_RESPONSES_API_KEY".to_owned(),
                "test-model-key".to_owned(),
            ),
            (
                "KITEFRAME_RESPONSES_MODEL".to_owned(),
                "test-model".to_owned(),
            ),
        ]);
        configured.extend(
            values
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned())),
        );
        Self::from_values(|name| configured.get(name).cloned())
    }

    #[must_use]
    pub fn external_harness_commands(&self) -> ExternalHarnessCommands {
        self.external_harness_commands.clone()
    }

    #[must_use]
    pub fn harness_routes(&self) -> HarnessRouteTable {
        self.harness_routes.clone()
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
        let store_limits = StoreLimits {
            max_active_sessions_per_principal: parse_quota(
                value("KITEFRAME_MAX_ACTIVE_SESSIONS_PER_PRINCIPAL"),
                64,
            )?,
            max_active_runs_per_session: parse_quota(
                value("KITEFRAME_MAX_ACTIVE_RUNS_PER_SESSION"),
                32,
            )?,
            max_open_permission_requests_per_session: parse_quota(
                value("KITEFRAME_MAX_OPEN_PERMISSION_REQUESTS_PER_SESSION"),
                32,
            )?,
            agent_runtime: {
                let defaults = RuntimeCeilings::default();
                RuntimeCeilings {
                    max_snapshot_bytes: parse_agent_runtime_ceiling(
                        value("KITEFRAME_AGENT_MAX_SNAPSHOT_BYTES"),
                        defaults.max_snapshot_bytes,
                    )?,
                    max_command_bytes: parse_agent_runtime_ceiling(
                        value("KITEFRAME_AGENT_MAX_COMMAND_BYTES"),
                        defaults.max_command_bytes,
                    )?,
                    max_commands_per_transition: parse_agent_runtime_ceiling(
                        value("KITEFRAME_AGENT_MAX_COMMANDS_PER_TRANSITION"),
                        defaults.max_commands_per_transition,
                    )?,
                    max_frames: parse_agent_runtime_ceiling(
                        value("KITEFRAME_AGENT_MAX_FRAMES"),
                        defaults.max_frames,
                    )?,
                    max_history_items_per_frame: parse_agent_runtime_ceiling(
                        value("KITEFRAME_AGENT_MAX_HISTORY_ITEMS_PER_FRAME"),
                        defaults.max_history_items_per_frame,
                    )?,
                }
            },
        };
        let external_harness_commands = ExternalHarnessCommands {
            codex: parse_external_command(
                value(CODEX_ACP_COMMAND),
                value(CODEX_ACP_SOURCE_REVISION),
                CODEX_ACP_COMMAND,
                CODEX_ACP_SOURCE_REVISION,
            )?,
            deepseek: parse_external_command(
                value(DEEPSEEK_ACP_COMMAND),
                value(DEEPSEEK_ACP_SOURCE_REVISION),
                DEEPSEEK_ACP_COMMAND,
                DEEPSEEK_ACP_SOURCE_REVISION,
            )?,
        };
        let harness_routes = HarnessRouteTable::new(
            parse_route(value("KITEFRAME_REASONING_HARNESS_PROFILE"))?,
            parse_route(value("KITEFRAME_CODING_HARNESS_PROFILE"))?,
        );

        Ok(Self {
            database_url,
            mode,
            local_principal,
            bearer_bindings,
            model,
            acp_v2_enabled,
            max_remote_connections,
            store_limits,
            external_harness_commands,
            harness_routes,
        })
    }
}

fn parse_route(value: Option<String>) -> Result<HarnessProfileId, ConfigError> {
    HarnessProfileId::new(value.unwrap_or_else(|| BUILTIN_HARNESS_PROFILE_ID.to_owned()))
        .map_err(|_| ConfigError::InvalidHarnessRoute)
}

fn parse_external_command(
    command: Option<String>,
    source_revision: Option<String>,
    command_name: &'static str,
    source_revision_name: &'static str,
) -> Result<Option<AcpCommand>, ConfigError> {
    match (command, source_revision) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(ConfigError::IncompleteExternalHarnessCommand {
            command_name,
            source_revision_name,
        }),
        (Some(command), Some(source_revision)) => {
            if !is_full_revision(&source_revision) {
                return Err(ConfigError::InvalidExternalHarnessSourceRevision {
                    source_revision_name,
                });
            }
            let command: Vec<String> = serde_json::from_str(&command)
                .map_err(|_| ConfigError::InvalidExternalHarnessCommand { command_name })?;
            let (program, args) = command
                .split_first()
                .ok_or(ConfigError::InvalidExternalHarnessCommand { command_name })?;
            if !valid_program(program) || !valid_literal_arguments(args) {
                return Err(ConfigError::InvalidExternalHarnessCommand { command_name });
            }
            Ok(Some(AcpCommand {
                program: program.into(),
                args: args.iter().map(OsString::from).collect(),
                source_revision,
            }))
        }
    }
}

fn is_full_revision(value: &str) -> bool {
    value.len() == 40
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !value.bytes().all(|byte| byte == b'0')
}

fn valid_program(value: &str) -> bool {
    if !valid_literal(value) {
        return false;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return true;
    }
    if path.components().count() != 1 {
        return false;
    }
    !matches!(
        value,
        "sh" | "bash" | "zsh" | "fish" | "cmd" | "powershell" | "pwsh"
    )
}

fn valid_literal_arguments(arguments: &[String]) -> bool {
    let mut workspace_override = false;
    for argument in arguments {
        if !valid_literal(argument) || contains_secret_flag(argument) {
            return false;
        }
        if workspace_override && !Path::new(argument).is_absolute() {
            return false;
        }
        workspace_override = is_workspace_override(argument);
        if let Some((flag, path)) = argument.split_once('=') {
            if is_workspace_override(flag) && !Path::new(path).is_absolute() {
                return false;
            }
        }
    }
    !workspace_override
}

fn valid_literal(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\0')
        && !value.contains([
            '\n', '\r', '|', '&', ';', '<', '>', '$', '`', '\'', '"', '*', '?',
        ])
}

fn contains_secret_flag(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "api-key",
        "api_key",
        "authorization",
        "bearer",
    ]
    .iter()
    .any(|name| normalized.starts_with(&format!("--{name}")))
}

fn is_workspace_override(value: &str) -> bool {
    matches!(value, "--dir" | "--cwd" | "--workspace" | "--prefix" | "-C")
}

fn parse_quota(value: Option<String>, default: usize) -> Result<usize, ConfigError> {
    let value = value
        .map_or(Ok(default), |value| value.parse::<usize>())
        .map_err(|_| ConfigError::InvalidDeploymentQuota)?;
    if !(1..=4096).contains(&value) {
        return Err(ConfigError::InvalidDeploymentQuota);
    }
    Ok(value)
}

fn parse_agent_runtime_ceiling(
    value: Option<String>,
    default: usize,
) -> Result<usize, ConfigError> {
    let value = value
        .map_or(Ok(default), |value| value.parse::<usize>())
        .map_err(|_| ConfigError::InvalidAgentRuntimeCeiling)?;
    if !(1..=default).contains(&value) {
        return Err(ConfigError::InvalidAgentRuntimeCeiling);
    }
    Ok(value)
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
    #[error("deployment quota values must be between 1 and 4096")]
    InvalidDeploymentQuota,
    #[error("agent runtime ceilings must be between 1 and their hard maximum")]
    InvalidAgentRuntimeCeiling,
    #[error("{command_name} and {source_revision_name} must be configured together")]
    IncompleteExternalHarnessCommand {
        command_name: &'static str,
        source_revision_name: &'static str,
    },
    #[error("{command_name} must be a safe non-empty JSON string array")]
    InvalidExternalHarnessCommand { command_name: &'static str },
    #[error("{source_revision_name} must be a nonzero full 40-hex commit hash")]
    InvalidExternalHarnessSourceRevision { source_revision_name: &'static str },
    #[error("configured harness route profile id is invalid")]
    InvalidHarnessRoute,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{ConfigError, HarnessConfig, HarnessMode};
    use crate::RuntimeCeilings;

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
    fn agent_runtime_ceilings_can_only_lower_the_hard_maxima() {
        let mut values = vec![
            ("DATABASE_URL", "postgres://localhost/kiteframe"),
            ("KITEFRAME_PRINCIPAL", "local-user"),
            ("KITEFRAME_AGENT_MAX_SNAPSHOT_BYTES", "131072"),
            ("KITEFRAME_AGENT_MAX_COMMAND_BYTES", "32768"),
            ("KITEFRAME_AGENT_MAX_COMMANDS_PER_TRANSITION", "32"),
            ("KITEFRAME_AGENT_MAX_FRAMES", "128"),
            ("KITEFRAME_AGENT_MAX_HISTORY_ITEMS_PER_FRAME", "128"),
        ];
        values.extend(model_values());
        assert_eq!(
            config(&values).unwrap().store_limits.agent_runtime,
            RuntimeCeilings {
                max_snapshot_bytes: 131_072,
                max_command_bytes: 32_768,
                max_commands_per_transition: 32,
                max_frames: 128,
                max_history_items_per_frame: 128,
            },
        );

        for (name, invalid) in [
            ("KITEFRAME_AGENT_MAX_SNAPSHOT_BYTES", "262145"),
            ("KITEFRAME_AGENT_MAX_COMMAND_BYTES", "65537"),
            ("KITEFRAME_AGENT_MAX_COMMANDS_PER_TRANSITION", "65"),
            ("KITEFRAME_AGENT_MAX_FRAMES", "257"),
            ("KITEFRAME_AGENT_MAX_HISTORY_ITEMS_PER_FRAME", "257"),
            ("KITEFRAME_AGENT_MAX_FRAMES", "0"),
            ("KITEFRAME_AGENT_MAX_FRAMES", "not-a-number"),
        ] {
            let mut invalid_values = vec![
                ("DATABASE_URL", "postgres://localhost/kiteframe"),
                ("KITEFRAME_PRINCIPAL", "local-user"),
                (name, invalid),
            ];
            invalid_values.extend(model_values());
            assert!(matches!(
                config(&invalid_values),
                Err(ConfigError::InvalidAgentRuntimeCeiling),
            ));
        }
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

    #[test]
    fn deployment_quotas_are_bounded_and_configurable() {
        let mut values = vec![
            ("DATABASE_URL", "postgres://localhost/kiteframe"),
            ("KITEFRAME_PRINCIPAL", "local-user"),
            ("KITEFRAME_MAX_ACTIVE_SESSIONS_PER_PRINCIPAL", "7"),
            ("KITEFRAME_MAX_ACTIVE_RUNS_PER_SESSION", "8"),
            ("KITEFRAME_MAX_OPEN_PERMISSION_REQUESTS_PER_SESSION", "9"),
        ];
        values.extend(model_values());
        let limits = config(&values).expect("config").store_limits;
        assert_eq!(limits.max_active_sessions_per_principal, 7);
        assert_eq!(limits.max_active_runs_per_session, 8);
        assert_eq!(limits.max_open_permission_requests_per_session, 9);
        values.retain(|(key, _)| *key != "KITEFRAME_MAX_ACTIVE_RUNS_PER_SESSION");
        values.push(("KITEFRAME_MAX_ACTIVE_RUNS_PER_SESSION", "0"));
        assert!(matches!(
            config(&values),
            Err(ConfigError::InvalidDeploymentQuota)
        ));
    }
}
