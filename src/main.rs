use std::{
    ffi::OsStr,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use agent_client_protocol::{ConnectTo, Stdio};
use kiteframe::config::{ExternalHarnessCommands, ExternalHarnessEnvironments};
use kiteframe::harness::{
    AcpHarnessPlugin, AcpLimits, DenyAllHarnessApprovals, HarnessProfile, HarnessRegistry,
    builtin_harness_profile,
};
use kiteframe::protocol::transport::{AcpService, BearerAuthenticator, RemoteService};
use kiteframe::store::HarnessPluginRegistration;
use kiteframe::{
    AgentCommandDispatcher, Application, AuthenticatedConnection, BuiltinHarnessPlugin, Engine,
    EngineConfig, HarnessConfig, HarnessMode, McpLimits, McpManager, ModelClient, ModelRequest,
    PersistedAuthorizer, PostgresStore, ResponsesClient, TransportKind,
};
use tokio::sync::watch;

const PROCESS_LOCK_KEY: i64 = 0x4b_49_54_45_46_52_41_4d;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const MODEL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MODEL_PROBE_INTERVAL: Duration = Duration::from_secs(30);

fn builtin_profile_from_attestation(
    source_revision: Option<&str>,
) -> Result<HarnessProfile, io::Error> {
    let source_revision = source_revision.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "KITEFRAME_SOURCE_REVISION must be set to the exact 40-hex Kiteframe commit at compile time",
        )
    })?;
    builtin_harness_profile(source_revision)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))
}

const CODEX_ACP_PROFILE: &[u8] = include_bytes!("../config/harness-profiles/codex-acp-v1.json");
const DEEPSEEK_ACP_PROFILE: &[u8] =
    include_bytes!("../config/harness-profiles/deepseek-harness-acp-v1.json");
const CODEX_ACP_REPOSITORY: &str = "https://github.com/zed-industries/codex-acp.git";
const CODEX_ACP_REVISION: &str = "296069e841634cd4bb9bc4515602d836e49231ec";
const CODEX_ACP_DIGEST: &str = "90a4c4b57334a1c75da6b45ab43c801ed38cece5f097231071215eeb77f660dd";
const DEEPSEEK_ACP_REPOSITORY: &str = "https://github.com/deepseek-ai/deepseek-harness.git";
const DEEPSEEK_ACP_REVISION: &str = "141eb6fef83422698aef7a981029e843e8161534";
const DEEPSEEK_ACP_DIGEST: &str =
    "1e9857d42a3f060be63bf67d09026c4ec7466d861a31ff1e52c205bc16c6e2b3";

fn external_profile(
    bytes: &[u8],
    id: &str,
    repository: &str,
    source_revision: &str,
    digest: &str,
) -> Result<HarnessProfile, io::Error> {
    let profile = HarnessProfile::from_json(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "harness_protocol_incompatible: checked-in external profile is invalid: {error}"
            ),
        )
    })?;
    if profile.id.as_str() != id
        || profile.source_repository != repository
        || profile.source_revision != source_revision
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "harness_protocol_incompatible: checked-in external profile attestation drifted",
        ));
    }
    let observed_digest = profile.digest().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "harness_protocol_incompatible: checked-in external profile digest failed: {error}"
            ),
        )
    })?;
    if observed_digest != digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "harness_protocol_incompatible: checked-in external profile digest drifted",
        ));
    }
    Ok(profile)
}

fn external_acp_profiles() -> Result<(HarnessProfile, HarnessProfile), io::Error> {
    Ok((
        external_profile(
            CODEX_ACP_PROFILE,
            "codex-acp-v1",
            CODEX_ACP_REPOSITORY,
            CODEX_ACP_REVISION,
            CODEX_ACP_DIGEST,
        )?,
        external_profile(
            DEEPSEEK_ACP_PROFILE,
            "deepseek-harness-acp-v1",
            DEEPSEEK_ACP_REPOSITORY,
            DEEPSEEK_ACP_REVISION,
            DEEPSEEK_ACP_DIGEST,
        )?,
    ))
}

fn external_acp_limits() -> AcpLimits {
    AcpLimits {
        max_frame_bytes: 1024 * 1024,
        shutdown_grace: SHUTDOWN_GRACE,
        max_pre_accept_attempts: 3,
    }
}

async fn register_profile(
    store: &PostgresStore,
    profile: &HarnessProfile,
) -> Result<(), Box<dyn std::error::Error>> {
    store
        .register_harness_plugin(HarnessPluginRegistration {
            profile_id: profile.id.clone(),
            profile_digest: profile.digest()?,
            source_repository: profile.source_repository.clone(),
            source_revision: profile.source_revision.clone(),
            transport: profile.transport,
            enabled: true,
        })
        .await?;
    Ok(())
}

async fn configured_harness_registry(
    store: Arc<PostgresStore>,
    model: Arc<dyn ModelClient>,
    builtin_profile: HarnessProfile,
    external_commands: ExternalHarnessCommands,
    external_environments: ExternalHarnessEnvironments,
) -> Result<Arc<HarnessRegistry>, Box<dyn std::error::Error>> {
    let mut registry = HarnessRegistry::new();
    registry
        .register(Arc::new(BuiltinHarnessPlugin::new(
            builtin_profile.clone(),
            model,
        )))
        .await?;
    register_profile(store.as_ref(), &builtin_profile).await?;

    let (codex_profile, deepseek_profile) = external_acp_profiles()?;
    for (profile, command, environment) in [
        (
            codex_profile,
            external_commands.codex,
            external_environments.codex,
        ),
        (
            deepseek_profile,
            external_commands.deepseek,
            external_environments.deepseek,
        ),
    ] {
        register_profile(store.as_ref(), &profile).await?;
        let Some(command) = command else {
            continue;
        };
        let plugin = AcpHarnessPlugin::with_environment(
            profile.clone(),
            command,
            external_acp_limits(),
            environment.map_or_else(Default::default, |environment| environment.values()),
        );
        let pre_accept_attempt_limit = plugin.pre_accept_attempt_limit();
        registry
            .register_with_pre_accept_attempt_limit(Arc::new(plugin), pre_accept_attempt_limit)
            .await?;
    }
    Ok(Arc::new(registry))
}

#[cfg(test)]
async fn configured_builtin_registry(
    store: Arc<PostgresStore>,
    model: Arc<dyn ModelClient>,
    profile: HarnessProfile,
) -> Result<Arc<HarnessRegistry>, Box<dyn std::error::Error>> {
    configured_harness_registry(
        store,
        model,
        profile,
        ExternalHarnessCommands::default(),
        ExternalHarnessEnvironments::default(),
    )
    .await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(version) = standalone_version(std::env::args_os()) {
        println!("{version}");
        return Ok(());
    }
    init_logging();
    let config = HarnessConfig::from_env()?;
    let external_harness_commands = config.external_harness_commands();
    let external_harness_environments = config.external_harness_environments();
    let builtin_profile =
        builtin_profile_from_attestation(option_env!("KITEFRAME_SOURCE_REVISION"))?;
    let harness_routes = config.harness_routes(builtin_profile.id.clone());
    let store = Arc::new(
        PostgresStore::connect_with_limits(&config.database_url, config.store_limits).await?,
    );
    let mut process_lock = ProcessLock::acquire(store.pool()).await?;
    let running = async move {
        let model = Arc::new(ResponsesClient::new(config.model.clone())?);
        let mcp = Arc::new(McpManager::with_store(McpLimits::default(), store.clone()));
        let engine = Arc::new(Engine {
            store: store.clone(),
            model: model.clone(),
            tools: mcp.clone(),
            authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
            config: EngineConfig::default(),
        });
        let recovery = engine.recover().await?;
        tracing::info!(
            model_attempts_requeued = recovery.model_attempts_requeued,
            tools_recovered = recovery.tools_recovered,
            tools_requeued = recovery.tools_requeued,
            sessions_waiting_for_mcp = recovery.sessions_waiting_for_mcp,
            outcomes_unknown = recovery.outcomes_unknown,
            "startup recovery complete"
        );
        let application = Application::new(store.clone(), engine.clone()).with_mcp_manager(mcp);
        let agent_recovery = application
            .agent_kernel()
            .recover(engine.config.scheduler_batch_size)
            .await?;
        tracing::info!(
            pending = agent_recovery.pending,
            claimed = agent_recovery.claimed,
            "agent command recovery complete"
        );
        let harness_registry = configured_harness_registry(
            store.clone(),
            model.clone(),
            builtin_profile,
            external_harness_commands,
            external_harness_environments,
        )
        .await?;
        let agent_dispatcher = Arc::new(application.agent_kernel().with_harness_registry_routes(
            harness_registry,
            Arc::new(DenyAllHarnessApprovals),
            harness_routes,
        ));
        let recovery_limit = engine.config.scheduler_batch_size.clamp(1, 256);
        let mut harness_recovery = kiteframe::HarnessRecoveryReport::default();
        loop {
            let page = agent_dispatcher
                .recover_harness_invocations(recovery_limit)
                .await?;
            let page_count = page.retryable_pre_accept + page.outcome_unknown + page.terminal;
            harness_recovery.retryable_pre_accept += page.retryable_pre_accept;
            harness_recovery.outcome_unknown += page.outcome_unknown;
            harness_recovery.terminal += page.terminal;
            if page_count < u64::try_from(recovery_limit).expect("positive recovery limit") {
                break;
            }
        }
        tracing::info!(
            retryable_pre_accept = harness_recovery.retryable_pre_accept,
            outcome_unknown = harness_recovery.outcome_unknown,
            terminal = harness_recovery.terminal,
            "harness invocation recovery complete"
        );
        let application = Arc::new(application);

        match config.mode {
            HarnessMode::Stdio => {
                let principal = config
                    .local_principal
                    .expect("validated stdio configuration has a local principal");
                run_stdio(
                    application,
                    engine,
                    agent_dispatcher,
                    principal,
                    config.acp_v2_enabled,
                )
                .await?;
            }
            HarnessMode::Server { bind } => {
                let max_remote_connections = config.max_remote_connections;
                let model_ready = Arc::new(AtomicBool::new(false));
                update_model_health(model.as_ref(), &model_ready).await;
                let _model_health_monitor =
                    ModelHealthMonitor::start(model.clone(), model_ready.clone());
                let authenticator = Arc::new(BearerAuthenticator::new(
                    config
                        .bearer_bindings
                        .into_iter()
                        .map(|binding| (binding.secret, binding.principal)),
                )?);
                let remote = RemoteService::with_v2_and_limit(
                    application,
                    authenticator,
                    store,
                    model_ready,
                    config.acp_v2_enabled,
                    max_remote_connections,
                );
                run_remote(bind, remote, engine, agent_dispatcher).await?;
            }
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => result?,
        error = process_lock.wait_for_loss() => return Err(error.into()),
    }
    Ok(())
}

fn standalone_version<I, S>(arguments: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut arguments = arguments.into_iter();
    let _program = arguments.next()?;
    match (arguments.next(), arguments.next()) {
        (Some(argument), None) if argument.as_ref() == OsStr::new("--version") => {
            Some(format!("kiteframe-harness {}", kiteframe::VERSION))
        }
        _ => None,
    }
}

struct ModelHealthMonitor {
    task: tokio::task::JoinHandle<()>,
}

impl ModelHealthMonitor {
    fn start(model: Arc<dyn ModelClient>, ready: Arc<AtomicBool>) -> Self {
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(MODEL_PROBE_INTERVAL).await;
                update_model_health(model.as_ref(), &ready).await;
            }
        });
        Self { task }
    }
}

impl Drop for ModelHealthMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn update_model_health(model: &dyn ModelClient, ready: &AtomicBool) {
    let request = ModelRequest {
        instructions: "Kiteframe readiness probe. Return a short text response.".to_owned(),
        input: vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Reply with ready."}]
        })],
        tools: Vec::new(),
    };
    let healthy = matches!(
        tokio::time::timeout(MODEL_PROBE_TIMEOUT, model.respond(request)).await,
        Ok(Ok(_))
    );
    ready.store(healthy, Ordering::Release);
    if !healthy {
        tracing::warn!("Responses provider readiness probe failed");
    }
}

fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter("info,agent_client_protocol=off,agent_client_protocol_http=off")
        .try_init();
}

struct ProcessLock {
    loss: watch::Receiver<Option<String>>,
    monitor: tokio::task::JoinHandle<()>,
}

impl ProcessLock {
    async fn acquire(pool: &sqlx::PgPool) -> Result<Self, io::Error> {
        let mut connection = pool
            .acquire()
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(PROCESS_LOCK_KEY)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        if !acquired {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "another Kiteframe harness holds the database process lock",
            ));
        }
        let (loss_tx, loss) = watch::channel(None);
        let monitor = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let probe = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&mut *connection);
                match tokio::time::timeout(Duration::from_secs(5), probe).await {
                    Ok(Ok(1)) => {}
                    Ok(Ok(_)) => {
                        loss_tx.send_replace(Some(
                            "database advisory-lock connection returned an invalid probe"
                                .to_owned(),
                        ));
                        return;
                    }
                    Ok(Err(error)) => {
                        loss_tx.send_replace(Some(format!(
                            "database advisory-lock connection was lost: {error}"
                        )));
                        return;
                    }
                    Err(_) => {
                        loss_tx.send_replace(Some(
                            "database advisory-lock connection probe timed out".to_owned(),
                        ));
                        return;
                    }
                }
            }
        });
        Ok(Self { loss, monitor })
    }

    async fn wait_for_loss(&mut self) -> io::Error {
        loop {
            if let Some(message) = self.loss.borrow().clone() {
                return io::Error::new(io::ErrorKind::ConnectionAborted, message);
            }
            if self.loss.changed().await.is_err() {
                return io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "database advisory-lock monitor stopped",
                );
            }
        }
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        self.monitor.abort();
    }
}

async fn run_stdio(
    application: Arc<Application>,
    engine: Arc<Engine>,
    agent_dispatcher: Arc<AgentCommandDispatcher>,
    principal: kiteframe::PrincipalId,
    v2_runtime_enabled: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let service = AcpService::new(
        application,
        engine.store.clone(),
        AuthenticatedConnection {
            principal,
            transport: TransportKind::Stdio,
        },
        v2_runtime_enabled,
    );
    let mut serving = Box::pin(service.clone().connect_to(Stdio::new()));
    let mut scheduling = Box::pin(run_scheduler(engine, agent_dispatcher, shutdown_rx));

    tokio::select! {
        result = &mut serving => {
            service.begin_shutdown();
            shutdown_tx.send_replace(true);
            result?;
            match tokio::time::timeout(SHUTDOWN_GRACE, &mut scheduling).await {
                Ok(result) => result?,
                Err(_) => tracing::warn!("scheduler exceeded shutdown grace period"),
            }
        }
        result = &mut scheduling => {
            service.begin_shutdown();
            shutdown_tx.send_replace(true);
            result?;
            match tokio::time::timeout(SHUTDOWN_GRACE, &mut serving).await {
                Ok(result) => result?,
                Err(_) => tracing::warn!("stdio ACP service exceeded shutdown grace period"),
            }
        }
        signal = shutdown_signal() => {
            signal?;
            service.begin_shutdown();
            shutdown_tx.send_replace(true);
            let (serving, scheduling) = tokio::join!(
                tokio::time::timeout(SHUTDOWN_GRACE, &mut serving),
                tokio::time::timeout(SHUTDOWN_GRACE, &mut scheduling),
            );
            match serving {
                Ok(result) => result?,
                Err(_) => tracing::warn!("stdio ACP service exceeded shutdown grace period"),
            }
            match scheduling {
                Ok(result) => result?,
                Err(_) => tracing::warn!("scheduler exceeded shutdown grace period"),
            }
        }
    }
    Ok(())
}

async fn run_remote(
    bind: std::net::SocketAddr,
    remote: RemoteService,
    engine: Arc<Engine>,
    agent_dispatcher: Arc<AgentCommandDispatcher>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local_address = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut server_shutdown = shutdown_rx.clone();
    let router = remote.router();
    let mut serving = Box::pin(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                wait_for_shutdown(&mut server_shutdown).await;
            })
            .await
    });
    let mut scheduling = Box::pin(run_scheduler(engine, agent_dispatcher, shutdown_rx));
    remote.mark_ready();
    tracing::info!(bind = %local_address, "remote ACP service ready");

    enum StopReason {
        Signal(io::Result<()>),
        Server(io::Result<()>),
        Scheduler(Result<(), SchedulerError>),
    }
    let reason = tokio::select! {
        signal = shutdown_signal() => StopReason::Signal(signal),
        result = &mut serving => StopReason::Server(result),
        result = &mut scheduling => StopReason::Scheduler(result),
    };
    remote.begin_shutdown();
    shutdown_tx.send_replace(true);

    match reason {
        StopReason::Signal(result) => {
            result?;
            let (server, scheduler) = tokio::join!(
                tokio::time::timeout(SHUTDOWN_GRACE, &mut serving),
                tokio::time::timeout(SHUTDOWN_GRACE, &mut scheduling),
            );
            match server {
                Ok(result) => result?,
                Err(_) => tracing::warn!("remote ACP service exceeded shutdown grace period"),
            }
            match scheduler {
                Ok(result) => result?,
                Err(_) => tracing::warn!("scheduler exceeded shutdown grace period"),
            }
        }
        StopReason::Server(result) => {
            result?;
            match tokio::time::timeout(SHUTDOWN_GRACE, &mut scheduling).await {
                Ok(result) => result?,
                Err(_) => tracing::warn!("scheduler exceeded shutdown grace period"),
            }
        }
        StopReason::Scheduler(result) => {
            result?;
            match tokio::time::timeout(SHUTDOWN_GRACE, &mut serving).await {
                Ok(result) => result?,
                Err(_) => tracing::warn!("remote ACP service exceeded shutdown grace period"),
            }
        }
    }
    tracing::info!("service shutdown complete");
    Ok(())
}

async fn shutdown_signal() -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

#[derive(Debug, thiserror::Error)]
enum SchedulerError {
    #[error(transparent)]
    Engine(#[from] kiteframe::EngineError),
    #[error(transparent)]
    Harness(#[from] kiteframe::HarnessDispatcherError),
}

impl SchedulerError {
    fn is_fatal(&self) -> bool {
        matches!(self, Self::Harness(error) if error.is_fatal())
    }
}

async fn run_scheduler(
    engine: Arc<Engine>,
    agent_dispatcher: Arc<AgentCommandDispatcher>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), SchedulerError> {
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let applied = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
            result = async {
                let legacy = engine.tick_until_idle().await?;
                let agents = agent_dispatcher
                    .tick_agent_commands(engine.config.scheduler_batch_size)
                    .await?;
                Ok::<usize, SchedulerError>(legacy + agents)
            } => match result {
                Ok(applied) => applied,
                Err(error) if error.is_fatal() => return Err(error),
                Err(error) => {
                    tracing::warn!(error = %error, "scheduler tick failed; retaining service availability");
                    0
                }
            },
        };
        if applied == 0 {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use kiteframe::dsl::{Limits, compile};
    use kiteframe::{
        AgentKernel, AgentSelector, CancelUserTask, ClaimAgentCommand, CreateSession, GoalContract,
        IdempotencyKey, KiteAgentRuntime, ModelError, ModelResponse, PrincipalId, RuntimeCeilings,
        SessionId, StartAgent, SubmitUserTask, SuccessCriterion, TaskOwnerAuthorizer,
        TaskPreferences, TaskTarget, ToolCatalogSnapshot, UserTaskId,
    };
    use sqlx::postgres::PgPoolOptions;
    use time::{Duration as TimeDuration, OffsetDateTime};
    use tokio::sync::oneshot;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn standalone_version_flag_reports_the_harness_package_version() {
        let arguments = ["kiteframe-harness", "--version"];

        assert_eq!(
            standalone_version(arguments.iter().copied()),
            Some(format!("kiteframe-harness {}", kiteframe::VERSION))
        );
        assert_eq!(standalone_version(["kiteframe-harness"].into_iter()), None);
    }

    #[test]
    fn builtin_startup_attestation_is_required_and_validated() {
        assert!(builtin_profile_from_attestation(None).is_err());
        assert!(builtin_profile_from_attestation(Some("invalid")).is_err());
        let profile =
            builtin_profile_from_attestation(Some("0123456789abcdef0123456789abcdef01234567"))
                .unwrap();
        assert_eq!(
            profile.source_revision,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(
            profile.id.as_str(),
            "builtin-v1-0123456789abcdef0123456789abcdef01234567"
        );
    }

    #[test]
    fn checked_in_external_profile_drift_is_a_protocol_incompatibility() {
        let error = external_profile(
            br#"{"id":"codex-acp-v1","transport":"acp_stdio"}"#,
            "codex-acp-v1",
            CODEX_ACP_REPOSITORY,
            CODEX_ACP_REVISION,
            CODEX_ACP_DIGEST,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("harness_protocol_incompatible:")
        );
    }

    #[tokio::test]
    async fn selected_external_profile_without_a_command_keeps_durable_profile_identity() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let database = IsolatedSchedulerDatabase::create(&database_url).await;
        let store = Arc::new(PostgresStore::connect(&database.url).await.unwrap());
        let builtin = builtin_harness_profile("0123456789abcdef0123456789abcdef01234567").unwrap();
        let model: Arc<dyn ModelClient> = Arc::new(RecoveringModel(AtomicUsize::new(0)));
        let registry = configured_harness_registry(
            store.clone(),
            model,
            builtin.clone(),
            ExternalHarnessCommands::default(),
            ExternalHarnessEnvironments::default(),
        )
        .await
        .unwrap();
        let selected = HarnessConfig::from_test_values([(
            "KITEFRAME_REASONING_HARNESS_PROFILE",
            "codex-acp-v1",
        )])
        .unwrap()
        .harness_routes(builtin.id)
        .reasoning;
        let (codex, deepseek) = external_acp_profiles().unwrap();

        assert_eq!(selected, codex.id);
        assert!(registry.profile_digest(&selected).is_err());
        assert_eq!(
            store.harness_plugin_registration(&selected).await.unwrap(),
            Some(HarnessPluginRegistration {
                profile_id: codex.id.clone(),
                profile_digest: codex.digest().unwrap(),
                source_repository: codex.source_repository.clone(),
                source_revision: codex.source_revision.clone(),
                transport: codex.transport,
                enabled: true,
            })
        );
        assert_eq!(
            store
                .harness_plugin_registration(&deepseek.id)
                .await
                .unwrap(),
            Some(HarnessPluginRegistration {
                profile_id: deepseek.id.clone(),
                profile_digest: deepseek.digest().unwrap(),
                source_repository: deepseek.source_repository.clone(),
                source_revision: deepseek.source_revision.clone(),
                transport: deepseek.transport,
                enabled: true,
            })
        );

        drop(registry);
        drop(store);
        database.drop().await;
    }

    struct RecoveringModel(AtomicUsize);

    #[async_trait]
    impl ModelClient for RecoveringModel {
        async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            if self.0.fetch_add(1, Ordering::AcqRel) == 0 {
                Err(ModelError::Unavailable("probe unavailable".to_owned()))
            } else {
                Ok(ModelResponse {
                    items: vec![serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "ready"}]
                    })],
                })
            }
        }
    }

    #[tokio::test]
    async fn model_readiness_recovers_from_an_independent_probe() {
        let model = RecoveringModel(AtomicUsize::new(0));
        let ready = AtomicBool::new(true);

        update_model_health(&model, &ready).await;
        assert!(!ready.load(Ordering::Acquire));

        update_model_health(&model, &ready).await;
        assert!(ready.load(Ordering::Acquire));
    }

    struct ActiveWorkModel {
        entered: Mutex<Option<oneshot::Sender<()>>>,
        cancelled: Arc<AtomicBool>,
    }

    struct MarkCancelled(Arc<AtomicBool>);

    impl Drop for MarkCancelled {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[async_trait]
    impl ModelClient for ActiveWorkModel {
        async fn respond(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
            if request
                .input
                .first()
                .and_then(|item| item["goal"]["objective"].as_str())
                != Some("Prove recovered scheduler shutdown cancellation.")
            {
                return Err(ModelError::Unavailable(
                    "non-target work is outside this scheduler test".to_owned(),
                ));
            }
            let _cancelled = MarkCancelled(self.cancelled.clone());
            if let Some(entered) = self.entered.lock().expect("entered lock").take() {
                let _ = entered.send(());
            }
            pending::<Result<ModelResponse, ModelError>>().await
        }
    }

    async fn scheduler_agent_fingerprint(
        store: &PostgresStore,
        task_id: &UserTaskId,
    ) -> serde_json::Value {
        sqlx::query_scalar(
            "SELECT jsonb_build_object( \
                 'task', (SELECT to_jsonb(t) FROM user_tasks t WHERE t.id = $1), \
                 'goal', (SELECT to_jsonb(g) FROM goal_states g WHERE g.task_id = $1), \
                 'commands', (SELECT COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.id), '[]'::jsonb) \
                     FROM agent_commands c WHERE c.task_id = $1), \
                 'events', (SELECT COALESCE(jsonb_agg(to_jsonb(e) ORDER BY e.sequence), '[]'::jsonb) \
                     FROM agent_task_events e WHERE e.task_id = $1))",
        )
        .bind(task_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .expect("fingerprint scheduler agent state")
    }

    struct AcceptedSchedulerAgent {
        principal: PrincipalId,
        session_id: SessionId,
        task_id: UserTaskId,
    }

    async fn accept_scheduler_agent(
        store: &PostgresStore,
        objective: &str,
        repository: &str,
        number: u64,
    ) -> AcceptedSchedulerAgent {
        let principal = PrincipalId::new(format!("scheduler-owner-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: format!("/scheduler-active-work/{number}"),
                retention_until: OffsetDateTime::now_utc() + TimeDuration::days(1),
            })
            .await
            .unwrap();
        let catalog = ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).unwrap();
        store.append_catalog_epoch(&catalog).await.unwrap();
        let name = format!("scheduler_agent_{}", Uuid::now_v7().simple());
        let source = format!(
            r#"agent {name}(repository: string, pull_request: integer) -> string {{
  entry inspect(repository = repository, pull_request = pull_request)
  limits {{ steps: 2 active_execution_seconds: 30 elapsed_seconds: 60 }}
  task inspect(repository: string, pull_request: integer) -> string {{
    model reasoning
    prompt """Wait until the scheduler is cancelled."""
    tools []
    on finished(summary: string) {{ done summary }}
  }}
}}"#
        );
        let compiled = compile(&source, &catalog, &Limits::default()).unwrap();
        let published = store
            .publish(session.id.clone(), &compiled, &source)
            .await
            .unwrap();
        let accepted = store
            .submit_user_task(
                &principal,
                SubmitUserTask {
                    session_id: session.id.clone(),
                    agent: AgentSelector {
                        name: published.name,
                        version: Some(published.version),
                    },
                    target: TaskTarget::GithubPullRequest {
                        repository: repository.to_owned(),
                        number,
                    },
                    preferences: TaskPreferences { harness_role: None },
                    goal: GoalContract {
                        version: 1,
                        objective: objective.to_owned(),
                        success: vec![SuccessCriterion {
                            id: "scheduler_test".to_owned(),
                            statement: "The scheduler test preserves its bounded invariant."
                                .to_owned(),
                            evidence: "harness.invocation_result".to_owned(),
                        }],
                        constraints: vec!["Do not wait without a timeout.".to_owned()],
                        blocked_when: vec!["The scheduler invariant cannot be proven.".to_owned()],
                    },
                    idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        AcceptedSchedulerAgent {
            principal,
            session_id: session.id,
            task_id: accepted.id,
        }
    }

    async fn start_scheduler_agent(kernel: &AgentKernel, accepted: &AcceptedSchedulerAgent) {
        kernel
            .start_agent(
                &accepted.principal,
                StartAgent {
                    session_id: accepted.session_id.clone(),
                    task_id: accepted.task_id.clone(),
                    expected_task_revision: 0,
                    expected_goal_revision: 0,
                    idempotency_key: IdempotencyKey::new(format!("start-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
    }

    struct IsolatedSchedulerDatabase {
        name: String,
        admin_url: String,
        url: String,
    }

    impl IsolatedSchedulerDatabase {
        async fn create(base_url: &str) -> Self {
            let mut admin = url::Url::parse(base_url).expect("database URL");
            admin.set_path("/postgres");
            let name = format!("kiteframe_scheduler_{}", Uuid::now_v7().simple());
            let admin_url = admin.to_string();
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&admin_url)
                .await
                .expect("connect database administrator");
            sqlx::query(&format!("CREATE DATABASE \"{name}\""))
                .execute(&pool)
                .await
                .expect("create isolated scheduler database");
            pool.close().await;
            let mut database = admin;
            database.set_path(&format!("/{name}"));
            Self {
                name,
                admin_url,
                url: database.to_string(),
            }
        }

        async fn drop(self) {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&self.admin_url)
                .await
                .expect("connect database administrator for scheduler cleanup");
            sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
                .execute(&pool)
                .await
                .expect("drop isolated scheduler database");
            pool.close().await;
        }
    }

    #[tokio::test]
    async fn scheduler_dispatches_recovered_claim_cancels_active_work_and_preserves_unrelated_work()
    {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let unrelated_store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let unrelated = accept_scheduler_agent(
            &unrelated_store,
            "Preserve unrelated scheduler work.",
            "acme/unrelated",
            141,
        )
        .await;
        let unrelated_kernel = Arc::new(AgentKernel::new(
            unrelated_store.clone(),
            Arc::new(KiteAgentRuntime::new(RuntimeCeilings::default())),
            Arc::new(TaskOwnerAuthorizer::new(unrelated_store.clone())),
        ));
        start_scheduler_agent(&unrelated_kernel, &unrelated).await;
        assert!(
            unrelated_store
                .dispatchable_agent_commands(256)
                .await
                .unwrap()
                .into_iter()
                .any(|command| command.task_id == unrelated.task_id),
            "unrelated command must be globally dispatchable before the scheduler runs"
        );
        let unrelated_before =
            scheduler_agent_fingerprint(&unrelated_store, &unrelated.task_id).await;

        let isolated_database = IsolatedSchedulerDatabase::create(&database_url).await;
        let store = Arc::new(
            PostgresStore::connect(&isolated_database.url)
                .await
                .unwrap(),
        );
        let accepted = accept_scheduler_agent(
            &store,
            "Prove recovered scheduler shutdown cancellation.",
            "acme/scheduler",
            142,
        )
        .await;
        let (entered_tx, entered_rx) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let gated = Arc::new(ActiveWorkModel {
            entered: Mutex::new(Some(entered_tx)),
            cancelled: cancelled.clone(),
        });
        let model: Arc<dyn ModelClient> = gated;
        let engine = Arc::new(Engine {
            store: store.clone(),
            model: model.clone(),
            tools: Arc::new(kiteframe::InProcessToolRegistry::new()),
            authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
            config: EngineConfig::default(),
        });
        let application = Application::new(store.clone(), engine.clone());
        let kernel = application.agent_kernel();
        start_scheduler_agent(&kernel, &accepted).await;
        let pending = store
            .dispatchable_agent_commands(engine.config.scheduler_batch_size)
            .await
            .unwrap()
            .into_iter()
            .find(|command| command.task_id == accepted.task_id)
            .expect("target command is dispatchable");
        let claimed = store
            .claim_agent_command(ClaimAgentCommand {
                session_id: accepted.session_id.clone(),
                task_id: accepted.task_id.clone(),
                command_id: pending.id.clone(),
                expected_command_revision: pending.revision,
                claimant: "harness-dispatcher-v1".to_owned(),
                idempotency_key: IdempotencyKey::new(format!(
                    "harness-dispatcher-v1:{}",
                    pending.id.as_uuid()
                ))
                .unwrap(),
            })
            .await
            .unwrap();
        let harness_registry = configured_builtin_registry(
            store.clone(),
            model,
            builtin_harness_profile("0123456789abcdef0123456789abcdef01234567").unwrap(),
        )
        .await
        .unwrap();
        let dispatcher = Arc::new(
            kernel
                .clone()
                .with_harness_registry(harness_registry, Arc::new(DenyAllHarnessApprovals)),
        );
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let scheduler = tokio::spawn(run_scheduler(engine, dispatcher, shutdown_rx));

        tokio::time::timeout(Duration::from_secs(3), entered_rx)
            .await
            .expect("recovered built-in claim must be dispatched")
            .expect("model entry sender must remain live");
        let persisted: (String, String, i64) =
            sqlx::query_as("SELECT state, claimant, revision FROM agent_commands WHERE id = $1")
                .bind(claimed.id.as_uuid())
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(persisted.0, "claimed");
        assert_eq!(persisted.1, "harness-dispatcher-v1");
        assert_eq!(persisted.2, i64::try_from(claimed.revision).unwrap());

        shutdown_tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), scheduler)
            .await
            .expect("scheduler shutdown is bounded")
            .expect("scheduler task joins")
            .expect("scheduler exits cleanly");
        assert!(cancelled.load(Ordering::Acquire));
        let after: (String, String, i64) =
            sqlx::query_as("SELECT state, claimant, revision FROM agent_commands WHERE id = $1")
                .bind(claimed.id.as_uuid())
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(after, persisted);
        assert_eq!(
            scheduler_agent_fingerprint(&unrelated_store, &unrelated.task_id).await,
            unrelated_before,
            "the scheduler test must not mutate unrelated eligible agent work"
        );
        unrelated_kernel
            .cancel_task(
                &unrelated.principal,
                CancelUserTask {
                    session_id: unrelated.session_id,
                    task_id: unrelated.task_id,
                    expected_task_revision: 1,
                    expected_goal_revision: 0,
                    reason: Some("Unrelated scheduler test cleanup.".to_owned()),
                    idempotency_key: IdempotencyKey::new(format!(
                        "cleanup-unrelated-{}",
                        Uuid::now_v7()
                    ))
                    .unwrap(),
                },
            )
            .await
            .unwrap();
        kernel
            .cancel_task(
                &accepted.principal,
                CancelUserTask {
                    session_id: accepted.session_id,
                    task_id: accepted.task_id,
                    expected_task_revision: 1,
                    expected_goal_revision: 0,
                    reason: Some("Scheduler test cleanup.".to_owned()),
                    idempotency_key: IdempotencyKey::new(format!("cleanup-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        store.pool().close().await;
        isolated_database.drop().await;
    }

    #[tokio::test]
    async fn scheduler_exits_fatally_when_owned_reservation_reconciliation_is_unavailable() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let database = IsolatedSchedulerDatabase::create(&database_url).await;
        let store = Arc::new(PostgresStore::connect(&database.url).await.unwrap());
        let accepted = accept_scheduler_agent(
            &store,
            "Fail fast when owned reconciliation is unavailable.",
            "acme/scheduler-fatal",
            143,
        )
        .await;
        let model = Arc::new(RecoveringModel(AtomicUsize::new(0)));
        let engine = Arc::new(Engine {
            store: store.clone(),
            model: model.clone(),
            tools: Arc::new(kiteframe::InProcessToolRegistry::new()),
            authorizer: Arc::new(PersistedAuthorizer::new(store.clone())),
            config: EngineConfig::default(),
        });
        let kernel = Application::new(store.clone(), engine.clone()).agent_kernel();
        start_scheduler_agent(&kernel, &accepted).await;
        let profile = builtin_harness_profile("0123456789abcdef0123456789abcdef01234567").unwrap();
        let registry = configured_builtin_registry(store.clone(), model.clone(), profile)
            .await
            .unwrap();
        let fault = Arc::new(kiteframe::HarnessDispatcherTestFault::new([
            kiteframe::HarnessDispatcherTestFaultPoint::WorkspaceLoad,
            kiteframe::HarnessDispatcherTestFaultPoint::Reconciliation,
        ]));
        let dispatcher = Arc::new(
            kernel
                .clone()
                .with_harness_registry(registry, Arc::new(DenyAllHarnessApprovals))
                .with_test_fault(fault.clone()),
        );
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);

        let error = tokio::time::timeout(
            Duration::from_secs(3),
            run_scheduler(engine, dispatcher, shutdown_rx),
        )
        .await
        .expect("fatal reconciliation failure stops the scheduler")
        .unwrap_err();
        assert!(matches!(
            error,
            SchedulerError::Harness(kiteframe::HarnessDispatcherError::ReconciliationFailed { .. })
        ));
        assert!(fault.fired(kiteframe::HarnessDispatcherTestFaultPoint::WorkspaceLoad));
        assert!(fault.fired(kiteframe::HarnessDispatcherTestFaultPoint::Reconciliation));
        assert_eq!(model.0.load(Ordering::Acquire), 0);
        let invocation_state: String = sqlx::query_scalar(
            "SELECT invocations.state FROM harness_invocations invocations \
             JOIN agent_commands commands ON commands.id = invocations.command_id \
             WHERE commands.task_id = $1",
        )
        .bind(accepted.task_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(invocation_state, "started");

        kernel
            .cancel_task(
                &accepted.principal,
                CancelUserTask {
                    session_id: accepted.session_id,
                    task_id: accepted.task_id,
                    expected_task_revision: 1,
                    expected_goal_revision: 0,
                    reason: Some("Scheduler fatal-path test cleanup.".to_owned()),
                    idempotency_key: IdempotencyKey::new(format!(
                        "cleanup-fatal-{}",
                        Uuid::now_v7()
                    ))
                    .unwrap(),
                },
            )
            .await
            .unwrap();
        store.pool().close().await;
        database.drop().await;
    }
}
