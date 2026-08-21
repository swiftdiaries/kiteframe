use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::super::{AcpCommand, AcpLimits, HarnessCapability};

const MAX_PENDING_REQUESTS: usize = 32;
const MAX_INBOUND_REQUESTS: usize = 32;
const SHUTDOWN_RUNNING: u8 = 0;
const SHUTDOWN_STDIN_CLOSED: u8 = 1;
const FRAME_QUEUED: u8 = 0;
const FRAME_WRITING: u8 = 1;
const FRAME_WRITTEN: u8 = 2;
const FRAME_CANCELLED: u8 = 3;

#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpTestBarrierPoint {
    BeforeAdmission,
    BeforeWrite,
    BeforeAcknowledgement,
}

#[cfg(feature = "test-support")]
pub struct AcpTestBarrier {
    method: &'static str,
    point: AcpTestBarrierPoint,
    entered: AtomicBool,
    released: AtomicBool,
    entered_notify: tokio::sync::Notify,
    release_notify: tokio::sync::Notify,
}

#[cfg(feature = "test-support")]
impl AcpTestBarrier {
    #[must_use]
    pub fn new(method: &'static str, point: AcpTestBarrierPoint) -> Self {
        Self {
            method,
            point,
            entered: AtomicBool::new(false),
            released: AtomicBool::new(false),
            entered_notify: tokio::sync::Notify::new(),
            release_notify: tokio::sync::Notify::new(),
        }
    }

    pub async fn wait_until_entered(&self) {
        while !self.entered.load(Ordering::Acquire) {
            self.entered_notify.notified().await;
        }
    }

    pub fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.release_notify.notify_waiters();
    }

    async fn pause(&self, method: &str, point: AcpTestBarrierPoint) {
        if self.method != method || self.point != point {
            return;
        }
        self.entered.store(true, Ordering::Release);
        self.entered_notify.notify_waiters();
        while !self.released.load(Ordering::Acquire) {
            self.release_notify.notified().await;
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AcpError {
    #[error("ACP protocol error: {0}")]
    Protocol(String),
    #[error("ACP process connection lost: {0}")]
    ConnectionLost(String),
    #[error("ACP request failed ({code}): {message}")]
    Remote { code: i64, message: String },
    #[error("ACP resource limit exceeded: {0}")]
    Limit(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum AcpInbound {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

enum WriterMessage {
    Frame {
        bytes: Vec<u8>,
        method: String,
        control: Arc<FrameControl>,
        written: oneshot::Sender<Result<(), AcpError>>,
    },
    Close,
}

#[derive(Default)]
struct FrameControl(AtomicU8);

impl FrameControl {
    fn cancel_before_write(&self) -> bool {
        self.0
            .compare_exchange(
                FRAME_QUEUED,
                FRAME_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

struct IoTasks {
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct ConnectionState {
    requests: Arc<PendingRequests>,
    inbound: mpsc::Sender<Result<AcpInbound, AcpError>>,
    cancellation: CancellationToken,
}

type PendingSender = oneshot::Sender<Result<Value, AcpError>>;

#[derive(Default)]
struct PendingRequestState {
    pending: BTreeMap<u64, PendingSender>,
    failure: Option<AcpError>,
}

#[derive(Default)]
struct PendingRequests(StdMutex<PendingRequestState>);

impl PendingRequests {
    fn admit(&self, id: u64, sender: PendingSender) -> Result<(), AcpError> {
        let mut state = self
            .0
            .lock()
            .expect("ACP pending request lock is not poisoned");
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.pending.len() >= MAX_PENDING_REQUESTS {
            return Err(AcpError::Limit("too many pending requests".to_owned()));
        }
        state.pending.insert(id, sender);
        Ok(())
    }

    fn remove(&self, id: u64) -> Option<PendingSender> {
        self.0
            .lock()
            .expect("ACP pending request lock is not poisoned")
            .pending
            .remove(&id)
    }

    fn failure(&self) -> Option<AcpError> {
        self.0
            .lock()
            .expect("ACP pending request lock is not poisoned")
            .failure
            .clone()
    }

    fn fail_once(&self, error: AcpError) -> Option<Vec<PendingSender>> {
        let mut state = self
            .0
            .lock()
            .expect("ACP pending request lock is not poisoned");
        if state.failure.is_some() {
            return None;
        }
        state.failure = Some(error);
        Some(std::mem::take(&mut state.pending).into_values().collect())
    }

    fn fail_and_drain(&self, error: AcpError) -> Vec<PendingSender> {
        let mut state = self
            .0
            .lock()
            .expect("ACP pending request lock is not poisoned");
        if state.failure.is_none() {
            state.failure = Some(error);
        }
        std::mem::take(&mut state.pending).into_values().collect()
    }
}

struct PendingRequestGuard {
    id: u64,
    requests: Arc<PendingRequests>,
}

pub(crate) struct AcpPreparedRequest {
    writer: mpsc::Sender<WriterMessage>,
    requests: Arc<PendingRequests>,
    response: oneshot::Receiver<Result<Value, AcpError>>,
    bytes: Option<Vec<u8>>,
    method: &'static str,
    control: Arc<FrameControl>,
    _pending: PendingRequestGuard,
}

impl AcpPreparedRequest {
    pub(crate) async fn write(&mut self) -> Result<(), AcpError> {
        let bytes = self
            .bytes
            .take()
            .ok_or_else(|| AcpError::Protocol("ACP request was already written".to_owned()))?;
        write_frame(
            &self.writer,
            bytes,
            self.method.to_owned(),
            self.control.clone(),
            &self.requests,
        )
        .await
    }

    pub(crate) fn cancel_before_write(&self) -> bool {
        self.control.cancel_before_write()
    }

    pub(crate) async fn response<R>(self) -> Result<R, AcpError>
    where
        R: DeserializeOwned,
    {
        let value = match self.response.await {
            Ok(result) => result?,
            Err(_) => {
                return Err(self.requests.failure().unwrap_or_else(|| {
                    AcpError::ConnectionLost("ACP connection closed".to_owned())
                }));
            }
        };
        serde_json::from_value(value).map_err(|error| AcpError::Protocol(error.to_string()))
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        self.requests.remove(self.id);
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum JsonRpcRequestId {
    Null,
    Number(i64),
    String(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InboundRequestState {
    Pending,
    Responded,
}

pub struct AcpStdioClient {
    writer: mpsc::Sender<WriterMessage>,
    inbound: AsyncMutex<mpsc::Receiver<Result<AcpInbound, AcpError>>>,
    requests: Arc<PendingRequests>,
    inbound_requests: Arc<std::sync::Mutex<BTreeMap<JsonRpcRequestId, InboundRequestState>>>,
    next_id: AtomicU64,
    child: AsyncMutex<Child>,
    connection_cancel: CancellationToken,
    shutdown_lock: AsyncMutex<()>,
    io_tasks: StdMutex<Option<IoTasks>>,
    limits: AcpLimits,
    #[cfg(feature = "test-support")]
    test_barrier: Option<Arc<AcpTestBarrier>>,
}

impl AcpStdioClient {
    pub async fn spawn(
        command: &AcpCommand,
        workspace: &Path,
        limits: AcpLimits,
        environment: &BTreeMap<OsString, OsString>,
    ) -> Result<Self, AcpError> {
        Self::spawn_inner(command, workspace, limits, environment, None).await
    }

    #[cfg(feature = "test-support")]
    pub async fn spawn_with_test_barrier(
        command: &AcpCommand,
        workspace: &Path,
        limits: AcpLimits,
        environment: &BTreeMap<OsString, OsString>,
        barrier: Arc<AcpTestBarrier>,
    ) -> Result<Self, AcpError> {
        Self::spawn_inner(command, workspace, limits, environment, Some(barrier)).await
    }

    async fn spawn_inner(
        command: &AcpCommand,
        workspace: &Path,
        limits: AcpLimits,
        environment: &BTreeMap<OsString, OsString>,
        #[cfg(feature = "test-support")] test_barrier: Option<Arc<AcpTestBarrier>>,
        #[cfg(not(feature = "test-support"))] _test_barrier: Option<Arc<()>>,
    ) -> Result<Self, AcpError> {
        if !workspace.is_absolute() {
            return Err(AcpError::Protocol(
                "ACP workspace must be absolute".to_owned(),
            ));
        }
        if limits.max_frame_bytes == 0 {
            return Err(AcpError::Limit("max_frame_bytes is zero".to_owned()));
        }
        let mut process = Command::new(&command.program);
        process
            .args(&command.args)
            .current_dir(workspace)
            .env_clear()
            .envs(environment)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = process
            .spawn()
            .map_err(|error| AcpError::ConnectionLost(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AcpError::ConnectionLost("ACP child stdin was not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AcpError::ConnectionLost("ACP child stdout was not piped".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AcpError::ConnectionLost("ACP child stderr was not piped".to_owned()))?;
        let (writer, writer_rx) = mpsc::channel::<WriterMessage>(MAX_PENDING_REQUESTS);
        let (inbound_tx, inbound) =
            mpsc::channel::<Result<AcpInbound, AcpError>>(MAX_PENDING_REQUESTS);
        let requests = Arc::new(PendingRequests::default());
        let inbound_requests = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let connection_cancel = CancellationToken::new();
        let shutdown_phase = Arc::new(AtomicU8::new(SHUTDOWN_RUNNING));
        let connection = ConnectionState {
            requests: requests.clone(),
            inbound: inbound_tx,
            cancellation: connection_cancel.clone(),
        };

        let writer_task = tokio::spawn(writer_task(
            stdin,
            writer_rx,
            connection.clone(),
            shutdown_phase.clone(),
            #[cfg(feature = "test-support")]
            test_barrier.clone(),
        ));
        let reader_task = tokio::spawn(reader_task(
            stdout,
            limits.max_frame_bytes,
            connection,
            inbound_requests.clone(),
            shutdown_phase.clone(),
        ));
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stderr, &mut sink).await;
        });

        Ok(Self {
            writer,
            inbound: AsyncMutex::new(inbound),
            requests,
            inbound_requests,
            next_id: AtomicU64::new(1),
            child: AsyncMutex::new(child),
            connection_cancel,
            shutdown_lock: AsyncMutex::new(()),
            io_tasks: StdMutex::new(Some(IoTasks {
                writer: Some(writer_task),
                reader: Some(reader_task),
                stderr: Some(stderr_task),
            })),
            limits,
            #[cfg(feature = "test-support")]
            test_barrier,
        })
    }

    pub async fn request<P, R>(&self, method: &'static str, params: P) -> Result<R, AcpError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let mut request = self.prepare_request(method, params).await?;
        request.write().await?;
        request.response().await
    }

    pub async fn initialize(&self) -> Result<InitializeResponse, AcpError> {
        self.request("initialize", InitializeRequest::stable())
            .await
    }

    pub async fn new_session(&self, cwd: PathBuf) -> Result<NewSessionResponse, AcpError> {
        self.request(
            "session/new",
            NewSessionRequest {
                cwd,
                additional_directories: vec![],
                mcp_servers: vec![],
            },
        )
        .await
    }

    pub async fn prompt(
        &self,
        session_id: String,
        text: String,
    ) -> Result<PromptResponse, AcpError> {
        let mut request = self.prepare_prompt(session_id, text).await?;
        request.write().await?;
        request.response().await
    }

    pub(crate) async fn prepare_prompt(
        &self,
        session_id: String,
        text: String,
    ) -> Result<AcpPreparedRequest, AcpError> {
        self.prepare_request(
            "session/prompt",
            PromptRequest {
                session_id,
                prompt: vec![TextContent {
                    content_type: "text",
                    text,
                }],
            },
        )
        .await
    }

    async fn prepare_request<P>(
        &self,
        method: &'static str,
        params: P,
    ) -> Result<AcpPreparedRequest, AcpError>
    where
        P: Serialize,
    {
        #[cfg(feature = "test-support")]
        if let Some(barrier) = &self.test_barrier {
            barrier
                .pause(method, AcpTestBarrierPoint::BeforeAdmission)
                .await;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
        let mut bytes =
            serde_json::to_vec(&frame).map_err(|error| AcpError::Protocol(error.to_string()))?;
        if bytes.len() > self.limits.max_frame_bytes {
            return Err(AcpError::Limit("outbound frame is too large".to_owned()));
        }
        bytes.push(b'\n');
        let (sender, response) = oneshot::channel();
        self.requests.admit(id, sender)?;
        Ok(AcpPreparedRequest {
            writer: self.writer.clone(),
            requests: self.requests.clone(),
            response,
            bytes: Some(bytes),
            method,
            control: Arc::new(FrameControl::default()),
            _pending: PendingRequestGuard {
                id,
                requests: self.requests.clone(),
            },
        })
    }

    pub async fn cancel(&self, session_id: String) -> Result<(), AcpError> {
        self.write_json(&json!({
            "jsonrpc":"2.0",
            "method":"session/cancel",
            "params": CancelNotification { session_id },
        }))
        .await
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<(), AcpError> {
        self.claim_inbound_response(&id)?;
        self.write_json(&json!({"jsonrpc":"2.0", "id":id, "result":result}))
            .await
    }

    pub async fn respond_error(
        &self,
        id: Value,
        code: i64,
        message: impl Into<String>,
    ) -> Result<(), AcpError> {
        self.claim_inbound_response(&id)?;
        self.write_json(&json!({
            "jsonrpc":"2.0",
            "id":id,
            "error":{"code":code,"message":message.into()}
        }))
        .await
    }

    pub async fn recv(&self) -> Result<AcpInbound, AcpError> {
        if let Some(message) = self.inbound.lock().await.recv().await {
            return message;
        }
        Err(self.classify_process_loss().await)
    }

    pub async fn try_recv(&self) -> Result<Option<AcpInbound>, AcpError> {
        match self.inbound.lock().await.try_recv() {
            Ok(Ok(message)) => Ok(Some(message)),
            Ok(Err(error)) => Err(error),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                Ok(None)
            }
        }
    }

    pub(crate) async fn verify_alive_before_shutdown(&self) -> Result<(), AcpError> {
        let settle = self.limits.shutdown_grace.min(Duration::from_millis(10));
        tokio::select! {
            biased;
            _ = self.connection_cancel.cancelled() => {
                return Err(self.classify_process_loss().await);
            }
            _ = tokio::time::sleep(settle) => {}
        }
        if let Some(error) = self.requests.failure() {
            return Err(error);
        }
        let mut child = self.child.lock().await;
        match child
            .try_wait()
            .map_err(|error| AcpError::ConnectionLost(error.to_string()))?
        {
            Some(status) => Err(AcpError::ConnectionLost(format!(
                "ACP process exited before shutdown with status {status}"
            ))),
            None => Ok(()),
        }
    }

    pub async fn shutdown(&self) -> Result<(), AcpError> {
        let _shutdown = self.shutdown_lock.lock().await;
        let deadline = tokio::time::Instant::now() + self.limits.shutdown_grace;
        let mut tasks = self.take_io_tasks();
        let graceful = self.graceful_shutdown(deadline, &mut tasks).await;
        if let Err(error) = graceful {
            self.force_cleanup(&mut tasks, error.clone()).await;
            return Err(error);
        }
        Ok(())
    }

    pub async fn kill(&self) {
        let _shutdown = self.shutdown_lock.lock().await;
        let mut tasks = self.take_io_tasks();
        self.force_cleanup(
            &mut tasks,
            AcpError::ConnectionLost("ACP process was killed".to_owned()),
        )
        .await;
    }

    async fn write_json(&self, value: &Value) -> Result<(), AcpError> {
        let mut bytes =
            serde_json::to_vec(value).map_err(|error| AcpError::Protocol(error.to_string()))?;
        if bytes.len() > self.limits.max_frame_bytes {
            return Err(AcpError::Limit("outbound frame is too large".to_owned()));
        }
        bytes.push(b'\n');
        write_frame(
            &self.writer,
            bytes,
            value
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("response")
                .to_owned(),
            Arc::new(FrameControl::default()),
            &self.requests,
        )
        .await
    }

    async fn classify_process_loss(&self) -> AcpError {
        self.requests
            .failure()
            .unwrap_or_else(|| AcpError::ConnectionLost("ACP connection closed".to_owned()))
    }

    fn claim_inbound_response(&self, id: &Value) -> Result<(), AcpError> {
        let id = json_rpc_request_id(id)?;
        let mut requests = self
            .inbound_requests
            .lock()
            .expect("ACP inbound request ledger lock is not poisoned");
        match requests.get_mut(&id) {
            Some(state @ InboundRequestState::Pending) => {
                *state = InboundRequestState::Responded;
                Ok(())
            }
            Some(InboundRequestState::Responded) => Err(AcpError::Protocol(
                "ACP inbound request already received a response".to_owned(),
            )),
            None => Err(AcpError::Protocol(
                "ACP response targets an unknown inbound request".to_owned(),
            )),
        }
    }

    fn take_io_tasks(&self) -> IoTasks {
        self.io_tasks
            .lock()
            .expect("ACP I/O task lock is not poisoned")
            .take()
            .unwrap_or(IoTasks {
                writer: None,
                reader: None,
                stderr: None,
            })
    }

    async fn graceful_shutdown(
        &self,
        deadline: tokio::time::Instant,
        tasks: &mut IoTasks,
    ) -> Result<(), AcpError> {
        if let Some(error) = self.requests.failure() {
            return Err(error);
        }
        tokio::time::timeout_at(deadline, self.writer.send(WriterMessage::Close))
            .await
            .map_err(|_| AcpError::Limit("ACP close enqueue exceeded shutdown grace".to_owned()))?
            .map_err(|_| AcpError::ConnectionLost("ACP writer task closed".to_owned()))?;
        join_before_deadline(&mut tasks.writer, deadline, "writer").await?;

        let status = {
            let mut child = self.child.lock().await;
            tokio::time::timeout_at(deadline, child.wait())
                .await
                .map_err(|_| AcpError::Limit("ACP child wait exceeded shutdown grace".to_owned()))?
                .map_err(|error| AcpError::ConnectionLost(error.to_string()))?
        };
        if !status.success() {
            return Err(AcpError::ConnectionLost(format!(
                "ACP process exited with nonzero status {status}"
            )));
        }
        join_before_deadline(&mut tasks.reader, deadline, "reader").await?;
        join_before_deadline(&mut tasks.stderr, deadline, "stderr").await?;
        if let Some(error) = self.requests.failure() {
            return Err(error);
        }
        Ok(())
    }

    async fn force_cleanup(&self, tasks: &mut IoTasks, error: AcpError) {
        self.connection_cancel.cancel();
        for sender in self.requests.fail_and_drain(error.clone()) {
            let _ = sender.send(Err(error.clone()));
        }
        {
            let mut child = self.child.lock().await;
            let _ = child.start_kill();
            let _ = tokio::time::timeout(self.limits.shutdown_grace, child.wait()).await;
        }
        abort_and_join(&mut tasks.writer).await;
        abort_and_join(&mut tasks.reader).await;
        abort_and_join(&mut tasks.stderr).await;
    }
}

impl Drop for AcpStdioClient {
    fn drop(&mut self) {
        self.connection_cancel.cancel();
        if let Some(mut tasks) = self
            .io_tasks
            .lock()
            .expect("ACP I/O task lock is not poisoned")
            .take()
        {
            abort_task(&mut tasks.writer);
            abort_task(&mut tasks.reader);
            abort_task(&mut tasks.stderr);
        }
    }
}

async fn join_before_deadline(
    task: &mut Option<JoinHandle<()>>,
    deadline: tokio::time::Instant,
    name: &'static str,
) -> Result<(), AcpError> {
    let Some(mut handle) = task.take() else {
        return Ok(());
    };
    match tokio::time::timeout_at(deadline, &mut handle).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(AcpError::ConnectionLost(format!(
            "ACP {name} task failed: {error}"
        ))),
        Err(_) => {
            *task = Some(handle);
            Err(AcpError::Limit(format!(
                "ACP {name} task exceeded shutdown grace"
            )))
        }
    }
}

fn abort_task(task: &mut Option<JoinHandle<()>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

async fn abort_and_join(task: &mut Option<JoinHandle<()>>) {
    if let Some(task) = task.take() {
        task.abort();
        let _ = tokio::time::timeout(Duration::from_millis(100), task).await;
    }
}

async fn write_frame(
    writer: &mpsc::Sender<WriterMessage>,
    bytes: Vec<u8>,
    method: String,
    control: Arc<FrameControl>,
    requests: &Arc<PendingRequests>,
) -> Result<(), AcpError> {
    let (written, acknowledgement) = oneshot::channel();
    writer
        .send(WriterMessage::Frame {
            bytes,
            method,
            control,
            written,
        })
        .await
        .map_err(|_| {
            requests
                .failure()
                .unwrap_or_else(|| AcpError::ConnectionLost("ACP writer task closed".to_owned()))
        })?;
    acknowledgement.await.map_err(|_| {
        requests.failure().unwrap_or_else(|| {
            AcpError::ConnectionLost("ACP writer acknowledgement closed".to_owned())
        })
    })?
}

async fn writer_task(
    mut stdin: tokio::process::ChildStdin,
    mut receiver: mpsc::Receiver<WriterMessage>,
    connection: ConnectionState,
    shutdown_phase: Arc<AtomicU8>,
    #[cfg(feature = "test-support")] test_barrier: Option<Arc<AcpTestBarrier>>,
) {
    loop {
        let message = tokio::select! {
            _ = connection.cancellation.cancelled() => return,
            message = receiver.recv() => match message {
                Some(message) => message,
                None => return,
            },
        };
        let (result, written) = match message {
            WriterMessage::Frame {
                bytes,
                method,
                control,
                written,
            } => {
                let _ = &method;
                #[cfg(feature = "test-support")]
                if let Some(barrier) = &test_barrier {
                    barrier
                        .pause(&method, AcpTestBarrierPoint::BeforeWrite)
                        .await;
                }
                if control
                    .0
                    .compare_exchange(
                        FRAME_QUEUED,
                        FRAME_WRITING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    let _ = written.send(Err(AcpError::Protocol(
                        "ACP frame was cancelled before write".to_owned(),
                    )));
                    continue;
                }
                let result = async {
                    stdin.write_all(&bytes).await?;
                    stdin.flush().await
                }
                .await;
                #[cfg(feature = "test-support")]
                if result.is_ok()
                    && let Some(barrier) = &test_barrier
                {
                    barrier
                        .pause(&method, AcpTestBarrierPoint::BeforeAcknowledgement)
                        .await;
                }
                if result.is_ok() {
                    control.0.store(FRAME_WRITTEN, Ordering::Release);
                }
                (result, Some(written))
            }
            WriterMessage::Close => {
                let result = async {
                    stdin.flush().await?;
                    shutdown_phase.store(SHUTDOWN_STDIN_CLOSED, Ordering::Release);
                    stdin.shutdown().await
                }
                .await;
                if result.is_ok() {
                    return;
                }
                (result, None)
            }
        };
        if let Some(written) = written {
            let _ = written.send(
                result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|error| AcpError::ConnectionLost(error.to_string())),
            );
        }
        if let Err(error) = result {
            fail_connection(AcpError::ConnectionLost(error.to_string()), &connection).await;
            return;
        }
    }
}

async fn reader_task(
    stdout: tokio::process::ChildStdout,
    max_frame_bytes: usize,
    connection: ConnectionState,
    inbound_requests: Arc<std::sync::Mutex<BTreeMap<JsonRpcRequestId, InboundRequestState>>>,
    shutdown_phase: Arc<AtomicU8>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let frame = tokio::select! {
            _ = connection.cancellation.cancelled() => return,
            frame = read_bounded_frame(&mut reader, max_frame_bytes) => frame,
        };
        match frame {
            Ok(Some(bytes)) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(frame) => {
                    if let Err(error) = dispatch_frame(
                        frame,
                        &connection.requests,
                        &inbound_requests,
                        &connection.inbound,
                    )
                    .await
                    {
                        fail_connection(error, &connection).await;
                        return;
                    }
                }
                Err(error) => {
                    fail_connection(
                        AcpError::Protocol(format!("stdout is not valid JSON-RPC: {error}")),
                        &connection,
                    )
                    .await;
                    return;
                }
            },
            Ok(None) => {
                if shutdown_phase.load(Ordering::Acquire) == SHUTDOWN_STDIN_CLOSED {
                    return;
                }
                fail_connection(
                    AcpError::ConnectionLost("ACP process reached EOF".to_owned()),
                    &connection,
                )
                .await;
                return;
            }
            Err(error) => {
                fail_connection(error, &connection).await;
                return;
            }
        }
    }
}

async fn read_bounded_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max_frame_bytes: usize,
) -> Result<Option<Vec<u8>>, AcpError> {
    let mut frame = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| AcpError::ConnectionLost(error.to_string()))?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(AcpError::Protocol("ACP stdout ended mid-frame".to_owned()))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        if frame.len().saturating_add(take) > max_frame_bytes {
            return Err(AcpError::Limit("inbound frame is too large".to_owned()));
        }
        frame.extend_from_slice(&available[..take]);
        let consumed = take + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            if frame.is_empty() {
                return Err(AcpError::Protocol("empty ACP frame".to_owned()));
            }
            return Ok(Some(frame));
        }
    }
}

async fn dispatch_frame(
    frame: Value,
    requests: &Arc<PendingRequests>,
    inbound_requests: &Arc<std::sync::Mutex<BTreeMap<JsonRpcRequestId, InboundRequestState>>>,
    inbound: &mpsc::Sender<Result<AcpInbound, AcpError>>,
) -> Result<(), AcpError> {
    let object = frame
        .as_object()
        .ok_or_else(|| AcpError::Protocol("JSON-RPC frame must be an object".to_owned()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(AcpError::Protocol(
            "JSON-RPC version must be 2.0".to_owned(),
        ));
    }
    if let Some(method) = object.get("method").and_then(Value::as_str) {
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "method" | "params"))
        {
            return Err(AcpError::Protocol(
                "unknown JSON-RPC request field".to_owned(),
            ));
        }
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
        let message = if let Some(id) = object.get("id") {
            let typed_id = json_rpc_request_id(id)?;
            let mut requests = inbound_requests
                .lock()
                .expect("ACP inbound request ledger lock is not poisoned");
            if requests.contains_key(&typed_id) {
                return Err(AcpError::Protocol(
                    "duplicate ACP inbound request id".to_owned(),
                ));
            }
            if requests.len() >= MAX_INBOUND_REQUESTS {
                return Err(AcpError::Limit("too many ACP inbound requests".to_owned()));
            }
            requests.insert(typed_id, InboundRequestState::Pending);
            AcpInbound::Request {
                id: id.clone(),
                method: method.to_owned(),
                params,
            }
        } else {
            AcpInbound::Notification {
                method: method.to_owned(),
                params,
            }
        };
        inbound
            .send(Ok(message))
            .await
            .map_err(|_| AcpError::ConnectionLost("ACP inbound receiver closed".to_owned()))?;
        return Ok(());
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "result" | "error"))
    {
        return Err(AcpError::Protocol(
            "unknown JSON-RPC response field".to_owned(),
        ));
    }
    let id = object
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| AcpError::Protocol("response id is not an unsigned integer".to_owned()))?;
    let sender = requests
        .remove(id)
        .ok_or_else(|| AcpError::Protocol("duplicate or unknown response id".to_owned()))?;
    let result = match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(parse_remote_error(error)?),
        _ => Err(AcpError::Protocol(
            "response must contain exactly one of result or error".to_owned(),
        )),
    };
    let _ = sender.send(result);
    Ok(())
}

fn json_rpc_request_id(value: &Value) -> Result<JsonRpcRequestId, AcpError> {
    match value {
        Value::Null => Ok(JsonRpcRequestId::Null),
        Value::Number(value) => value.as_i64().map(JsonRpcRequestId::Number).ok_or_else(|| {
            AcpError::Protocol("JSON-RPC numeric request id must fit signed int64".to_owned())
        }),
        Value::String(value) => Ok(JsonRpcRequestId::String(value.clone())),
        _ => Err(AcpError::Protocol(
            "JSON-RPC request id must be null, a signed integer, or a string".to_owned(),
        )),
    }
}

pub(crate) fn canonical_request_id(value: &Value) -> Result<String, AcpError> {
    Ok(match json_rpc_request_id(value)? {
        JsonRpcRequestId::Null => "null".to_owned(),
        JsonRpcRequestId::Number(value) => format!("number:{value}"),
        JsonRpcRequestId::String(value) => format!("string:{value}"),
    })
}

fn parse_remote_error(value: &Value) -> Result<AcpError, AcpError> {
    let object = value
        .as_object()
        .ok_or_else(|| AcpError::Protocol("JSON-RPC error must be an object".to_owned()))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "code" | "message" | "data"))
    {
        return Err(AcpError::Protocol(
            "unknown JSON-RPC error field".to_owned(),
        ));
    }
    Ok(AcpError::Remote {
        code: object
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(|| AcpError::Protocol("JSON-RPC error code is absent".to_owned()))?,
        message: object
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::Protocol("JSON-RPC error message is absent".to_owned()))?
            .to_owned(),
    })
}

async fn fail_connection(error: AcpError, connection: &ConnectionState) {
    let Some(drained) = connection.requests.fail_once(error.clone()) else {
        return;
    };
    connection.cancellation.cancel();
    for sender in drained {
        let _ = sender.send(Err(error.clone()));
    }
    let _ = connection.inbound.send(Err(error)).await;
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InitializeRequest {
    protocol_version: u16,
    client_capabilities: Value,
    client_info: Implementation,
}

impl InitializeRequest {
    fn stable() -> Self {
        Self {
            protocol_version: 1,
            client_capabilities: json!({"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}),
            client_info: Implementation {
                name: "kiteframe".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                title: None,
                meta: None,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InitializeResponse {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<Value>,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Implementation {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
    #[serde(default)]
    pub session_capabilities: Value,
    #[serde(default)]
    pub auth: Value,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

impl AgentCapabilities {
    pub fn optional_capabilities(&self) -> BTreeMap<HarnessCapability, bool> {
        BTreeMap::from([
            (HarnessCapability::SessionLoad, self.load_session),
            (
                HarnessCapability::EmbeddedContext,
                self.prompt_capabilities.embedded_context,
            ),
            (
                HarnessCapability::ImagePrompt,
                self.prompt_capabilities.image,
            ),
            (HarnessCapability::McpHttp, self.mcp_capabilities.http),
        ])
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewSessionRequest {
    cwd: PathBuf,
    additional_directories: Vec<PathBuf>,
    mcp_servers: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewSessionResponse {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<Value>,
    #[serde(default)]
    pub config_options: Option<Vec<Value>>,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromptRequest {
    session_id: String,
    prompt: Vec<TextContent>,
}

#[derive(Debug, Serialize)]
struct TextContent {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptResponse {
    pub stop_reason: StopReason,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CancelNotification {
    session_id: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    AgentMessageChunk {
        content: ContentBlock,
        #[serde(default)]
        message_id: Option<String>,
        #[serde(default, rename = "_meta")]
        meta: Option<Value>,
    },
    UserMessageChunk(Value),
    AgentThoughtChunk(Value),
    ToolCall(Value),
    ToolCallUpdate(Value),
    Plan(Value),
    AvailableCommandsUpdate(Value),
    CurrentModeUpdate(Value),
    ConfigOptionUpdate(Value),
    SessionInfoUpdate(Value),
    UsageUpdate(Value),
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(default)]
        annotations: Option<Value>,
        #[serde(default, rename = "_meta")]
        meta: Option<Value>,
    },
    Image {
        data: String,
        mime_type: String,
    },
    Audio {
        data: String,
        mime_type: String,
    },
    ResourceLink {
        uri: String,
        name: String,
    },
    Resource {
        resource: Value,
    },
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestPermission {
    pub session_id: String,
    pub tool_call: PermissionToolCall,
    pub options: Vec<WirePermissionOption>,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionToolCall {
    pub tool_call_id: String,
    #[serde(default)]
    pub kind: Option<Value>,
    #[serde(default)]
    pub status: Option<Value>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub locations: Option<Value>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WirePermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
    #[serde(default, rename = "_meta")]
    pub meta: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_wire_dtos_match_acp_v1_method_field_casing() {
        assert_eq!(
            serde_json::to_value(InitializeRequest::stable()).unwrap(),
            json!({
                "protocolVersion":1,
                "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},
                "clientInfo":{"name":"kiteframe","version":env!("CARGO_PKG_VERSION")},
            })
        );
        assert_eq!(
            serde_json::to_value(NewSessionRequest {
                cwd: PathBuf::from("/workspace"),
                additional_directories: vec![],
                mcp_servers: vec![],
            })
            .unwrap(),
            json!({"cwd":"/workspace","additionalDirectories":[],"mcpServers":[]})
        );
        assert_eq!(
            serde_json::to_value(PromptRequest {
                session_id: "session-1".to_owned(),
                prompt: vec![TextContent {
                    content_type: "text",
                    text: "hello".to_owned(),
                }],
            })
            .unwrap(),
            json!({"sessionId":"session-1","prompt":[{"type":"text","text":"hello"}]})
        );
        assert_eq!(
            serde_json::to_value(CancelNotification {
                session_id: "session-1".to_owned(),
            })
            .unwrap(),
            json!({"sessionId":"session-1"})
        );

        let initialize: InitializeResponse = serde_json::from_value(json!({
            "protocolVersion":1,
            "agentCapabilities":{},
            "agentInfo":{"name":"fixture","version":"1"}
        }))
        .unwrap();
        assert_eq!(initialize.protocol_version, 1);
        let session: NewSessionResponse =
            serde_json::from_value(json!({"sessionId":"session-1"})).unwrap();
        assert_eq!(session.session_id, "session-1");
        let prompt: PromptResponse =
            serde_json::from_value(json!({"stopReason":"end_turn"})).unwrap();
        assert_eq!(prompt.stop_reason, StopReason::EndTurn);

        let update: SessionNotification = serde_json::from_value(json!({
            "sessionId":"session-1",
            "update":{
                "sessionUpdate":"agent_message_chunk",
                "content":{"type":"text","text":"done"}
            }
        }))
        .unwrap();
        assert_eq!(update.session_id, "session-1");
        let permission: RequestPermission = serde_json::from_value(json!({
            "sessionId":"session-1",
            "toolCall":{"toolCallId":"tool-1","title":"Run"},
            "options":[{"optionId":"once","name":"Once","kind":"allow_once"}]
        }))
        .unwrap();
        assert_eq!(permission.options[0].option_id, "once");
    }
}
