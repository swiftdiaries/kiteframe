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
    AgentCommandDispatcher, Application, AuthenticatedConnection, DirectLoopHarnessExecutor,
    Engine, EngineConfig, HarnessConfig, HarnessMode, McpLimits, McpManager, ModelClient,
    ModelRequest, PersistedAuthorizer, PostgresStore, ResponsesClient, TransportKind,
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
        let agent_dispatcher = Arc::new(
            application
                .agent_kernel()
                .with_harness_executor(Arc::new(DirectLoopHarnessExecutor::new(model.clone()))),
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
    agent_dispatcher: Arc<AgentCommandDispatcher>,
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
            result = async {
                let legacy = engine.tick_until_idle().await?;
                let agents = agent_dispatcher
                    .tick_agent_commands(engine.config.scheduler_batch_size)
                    .await?;
                Ok::<usize, kiteframe::EngineError>(legacy + agents)
            } => match result {
                Ok(applied) => applied,
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
        AgentSelector, CancelUserTask, ClaimAgentCommand, CreateSession, GoalContract,
        IdempotencyKey, ModelError, ModelResponse, PrincipalId, StartAgent, SubmitUserTask,
        SuccessCriterion, TaskPreferences, TaskTarget, ToolCatalogSnapshot,
    };
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

    #[tokio::test]
    async fn scheduler_dispatches_recovered_builtin_claim_and_cancels_active_work_on_shutdown() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let store = Arc::new(PostgresStore::connect(&database_url).await.unwrap());
        let principal = PrincipalId::new(format!("scheduler-owner-{}", Uuid::now_v7())).unwrap();
        let session = store
            .create_session(CreateSession {
                principal: principal.clone(),
                working_directory: "/scheduler-active-work".to_owned(),
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
                        repository: "acme/scheduler".to_owned(),
                        number: 142,
                    },
                    preferences: TaskPreferences { harness_role: None },
                    goal: GoalContract {
                        version: 1,
                        objective: "Prove recovered scheduler shutdown cancellation.".to_owned(),
                        success: vec![SuccessCriterion {
                            id: "scheduler_cancelled".to_owned(),
                            statement: "Active scheduler work was cancelled on shutdown."
                                .to_owned(),
                            evidence: "harness.invocation_result".to_owned(),
                        }],
                        constraints: vec!["Do not wait without a timeout.".to_owned()],
                        blocked_when: vec!["The model gate is never entered.".to_owned()],
                    },
                    idempotency_key: IdempotencyKey::new(format!("submit-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
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
        kernel
            .start_agent(
                &principal,
                StartAgent {
                    session_id: session.id.clone(),
                    task_id: accepted.id.clone(),
                    expected_task_revision: 0,
                    expected_goal_revision: 0,
                    idempotency_key: IdempotencyKey::new(format!("start-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let pending = store
            .dispatchable_agent_commands(engine.config.scheduler_batch_size)
            .await
            .unwrap()
            .into_iter()
            .find(|command| command.task_id == accepted.id)
            .expect("target command is dispatchable");
        let claimed = store
            .claim_agent_command(ClaimAgentCommand {
                session_id: session.id.clone(),
                task_id: accepted.id.clone(),
                command_id: pending.id.clone(),
                expected_command_revision: pending.revision,
                claimant: "builtin-direct-v1".to_owned(),
                idempotency_key: IdempotencyKey::new(format!(
                    "builtin-direct-v1:{}",
                    pending.id.as_uuid()
                ))
                .unwrap(),
            })
            .await
            .unwrap();
        let dispatcher = Arc::new(
            kernel
                .clone()
                .with_harness_executor(Arc::new(kiteframe::DirectLoopHarnessExecutor::new(model))),
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
        assert_eq!(persisted.1, "builtin-direct-v1");
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
        kernel
            .cancel_task(
                &principal,
                CancelUserTask {
                    session_id: session.id,
                    task_id: accepted.id,
                    expected_task_revision: 1,
                    expected_goal_revision: 0,
                    reason: Some("Scheduler test cleanup.".to_owned()),
                    idempotency_key: IdempotencyKey::new(format!("cleanup-{}", Uuid::now_v7()))
                        .unwrap(),
                },
            )
            .await
            .unwrap();
    }
}
