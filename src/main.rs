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
use kiteframe::protocol::transport::{AcpService, BearerAuthenticator, RemoteService};
use kiteframe::{
    Application, AuthenticatedConnection, Engine, EngineConfig, HarnessConfig, HarnessMode,
    McpLimits, McpManager, ModelClient, ModelRequest, PersistedAuthorizer, PostgresStore,
    ResponsesClient, TransportKind,
};
use tokio::sync::watch;

const PROCESS_LOCK_KEY: i64 = 0x4b_49_54_45_46_52_41_4d;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const MODEL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MODEL_PROBE_INTERVAL: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(version) = standalone_version(std::env::args_os()) {
        println!("{version}");
        return Ok(());
    }
    init_logging();
    let config = HarnessConfig::from_env()?;
    let store = Arc::new(PostgresStore::connect(&config.database_url).await?);
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
        let application =
            Arc::new(Application::new(store.clone(), engine.clone()).with_mcp_manager(mcp));

        match config.mode {
            HarnessMode::Stdio => {
                let principal = config
                    .local_principal
                    .expect("validated stdio configuration has a local principal");
                run_stdio(application, engine, principal, config.acp_v2_enabled).await?;
            }
            HarnessMode::Server { bind } => {
                let max_remote_connections = config.max_remote_connections;
                let model_ready = Arc::new(AtomicBool::new(false));
                update_model_health(model.as_ref(), &model_ready).await;
                let _model_health_monitor = ModelHealthMonitor::start(model, model_ready.clone());
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
                run_remote(bind, remote, engine).await?;
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
    let mut scheduling = Box::pin(run_scheduler(engine, shutdown_rx));

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
    let mut scheduling = Box::pin(run_scheduler(engine, shutdown_rx));
    remote.mark_ready();
    tracing::info!(bind = %local_address, "remote ACP service ready");

    enum StopReason {
        Signal(io::Result<()>),
        Server(io::Result<()>),
        Scheduler(Result<(), kiteframe::EngineError>),
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

async fn run_scheduler(
    engine: Arc<Engine>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), kiteframe::EngineError> {
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
            result = engine.tick_until_idle() => result?,
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
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use kiteframe::{ModelError, ModelResponse};

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
}
