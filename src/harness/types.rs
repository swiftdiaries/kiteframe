use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{AgentCommandId, HarnessRole, InvokeHarness, ModelRole, RuntimeHistoryItem};

pub const HARNESS_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct HarnessProfileId(String);

impl HarnessProfileId {
    pub fn new(value: impl Into<String>) -> Result<Self, HarnessFailure> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "invalid harness profile id",
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for HarnessProfileId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessTransport {
    Builtin,
    AcpStdio,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessCapability {
    FreshSession,
    TextPrompt,
    PermissionOnce,
    Cancel,
    CommittedOutput,
    SessionLoad,
    EmbeddedContext,
    ImagePrompt,
    McpStdio,
    McpHttp,
    ToolPresentation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessCapabilities {
    pub protocol_version: u16,
    pub implementation_name: String,
    pub implementation_version: String,
    pub supported: std::collections::BTreeSet<HarnessCapability>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessApprovalRequest {
    pub invocation_id: HarnessInvocationId,
    pub request_id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessInvocationOutput {
    pub outcome: String,
    pub fields: serde_json::Value,
    pub history: Vec<RuntimeHistoryItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcpCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub source_revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcpLimits {
    pub max_frame_bytes: usize,
    pub shutdown_grace: Duration,
    pub max_pre_accept_attempts: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionOption {
    pub id: String,
    pub kind: String,
}
pub type PermissionOptionId = String;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HarnessInvocationId(Uuid);
impl HarnessInvocationId {
    pub fn new(value: Uuid) -> Self {
        Self(value)
    }
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}
impl Serialize for HarnessInvocationId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}
impl<'de> Deserialize<'de> for HarnessInvocationId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self(Uuid::deserialize(d)?))
    }
}

pub(crate) fn terminal_idempotency_key(invocation_id: &HarnessInvocationId) -> String {
    format!("harness-terminal:{}", invocation_id.as_uuid())
}

pub(crate) fn canonical_outcome_unknown_terminal_payload() -> Value {
    json!({
        "type": "outcome_unknown",
        "code": "harness_outcome_unknown",
        "detail": "harness terminal outcome is unknown",
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessInvocationRequest {
    pub invocation_id: HarnessInvocationId,
    pub command_id: AgentCommandId,
    pub command_revision: u64,
    pub profile_id: HarnessProfileId,
    pub workspace: PathBuf,
    pub payload: InvokeHarness,
    pub payload_digest: String,
}

#[derive(Debug)]
pub struct HarnessPendingEvent {
    pub event: HarnessInvocationEvent,
    pub persisted: oneshot::Sender<Result<(), HarnessFailure>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum HarnessInvocationEvent {
    Started,
    Progress {
        message: String,
    },
    ApprovalRequested {
        request_id: String,
        title: String,
    },
    ApprovalResolved {
        request_id: String,
        decision: HarnessApprovalDecision,
    },
    AssistantMessage {
        text: String,
    },
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessApprovalDecision {
    AllowOnce,
    RejectOnce,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HarnessTerminal {
    Completed(HarnessInvocationOutput),
    Cancelled,
    Retryable(HarnessFailure),
    Failed(HarnessFailure),
    OutcomeUnknown(HarnessFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessFailure {
    pub code: &'static str,
    pub detail: String,
}
impl HarnessFailure {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}
impl std::fmt::Display for HarnessFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}
impl std::error::Error for HarnessFailure {}

#[async_trait::async_trait]
pub trait HarnessAcceptanceBarrier: Send + Sync {
    async fn commit(&self) -> Result<(), HarnessFailure>;
}

#[derive(Clone, Debug)]
pub struct HarnessRouteTable {
    pub reasoning: HarnessProfileId,
    pub coding: HarnessProfileId,
}
impl HarnessRouteTable {
    pub fn new(reasoning: HarnessProfileId, coding: HarnessProfileId) -> Self {
        Self { reasoning, coding }
    }
    pub fn select(&self, role: Option<HarnessRole>, model_role: ModelRole) -> &HarnessProfileId {
        match role {
            Some(HarnessRole::Coding) => &self.coding,
            Some(HarnessRole::Reasoning) => &self.reasoning,
            None => match model_role {
                ModelRole::Coding => &self.coding,
                ModelRole::Reasoning => &self.reasoning,
            },
        }
    }
}

#[derive(Clone)]
pub struct HarnessEventSender {
    sender: mpsc::Sender<HarnessPendingEvent>,
    admission_open: std::sync::Arc<std::sync::Mutex<bool>>,
    acceptance_barrier: Arc<dyn HarnessAcceptanceBarrier>,
    acceptance_result: Arc<tokio::sync::OnceCell<Result<(), HarnessFailure>>>,
}

impl HarnessEventSender {
    pub(crate) fn new(
        sender: mpsc::Sender<HarnessPendingEvent>,
        acceptance_barrier: Arc<dyn HarnessAcceptanceBarrier>,
    ) -> Self {
        Self {
            sender,
            admission_open: std::sync::Arc::new(std::sync::Mutex::new(true)),
            acceptance_barrier,
            acceptance_result: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    pub async fn commit_acceptance(&self) -> Result<(), HarnessFailure> {
        self.acceptance_result
            .get_or_init(|| self.acceptance_barrier.commit())
            .await
            .clone()
    }

    pub(crate) fn acceptance_committed(&self) -> bool {
        matches!(self.acceptance_result.get(), Some(Ok(())))
    }

    pub async fn send(
        &self,
        pending: HarnessPendingEvent,
    ) -> Result<(), mpsc::error::SendError<HarnessPendingEvent>> {
        if !*self
            .admission_open
            .lock()
            .expect("harness event admission lock is not poisoned")
        {
            return Err(mpsc::error::SendError(pending));
        }
        self.sender.send(pending).await
    }

    pub(crate) fn close_admission(&self) {
        *self
            .admission_open
            .lock()
            .expect("harness event admission lock is not poisoned") = false;
    }
}

impl std::fmt::Debug for HarnessEventSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HarnessEventSender")
            .field("acceptance_committed", &self.acceptance_committed())
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)]
pub type HarnessCancellation = CancellationToken;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::{Notify, Semaphore, mpsc};

    use super::{HarnessAcceptanceBarrier, HarnessEventSender, HarnessFailure};

    struct BlockingSuccessBarrier {
        calls: Arc<AtomicUsize>,
        entered: Arc<Notify>,
        release: Arc<Semaphore>,
    }

    #[async_trait::async_trait]
    impl HarnessAcceptanceBarrier for BlockingSuccessBarrier {
        async fn commit(&self) -> Result<(), HarnessFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.release
                .acquire()
                .await
                .expect("test barrier remains open")
                .forget();
            Ok(())
        }
    }

    struct CountingErrorBarrier(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl HarnessAcceptanceBarrier for CountingErrorBarrier {
        async fn commit(&self) -> Result<(), HarnessFailure> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(HarnessFailure::new(
                "harness_outcome_unknown",
                "durable acceptance failed",
            ))
        }
    }

    struct CancelThenSucceedBarrier {
        calls: Arc<AtomicUsize>,
        first_entered: Arc<Notify>,
        hold_first: Arc<Semaphore>,
    }

    #[async_trait::async_trait]
    impl HarnessAcceptanceBarrier for CancelThenSucceedBarrier {
        async fn commit(&self) -> Result<(), HarnessFailure> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.first_entered.notify_one();
                self.hold_first
                    .acquire()
                    .await
                    .expect("test barrier remains open")
                    .forget();
            }
            Ok(())
        }
    }

    fn sender(barrier: Arc<dyn HarnessAcceptanceBarrier>) -> HarnessEventSender {
        let (events, _receiver) = mpsc::channel(1);
        HarnessEventSender::new(events, barrier)
    }

    #[tokio::test]
    async fn concurrent_clones_share_one_durable_acceptance_call_and_result() {
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let sender = sender(Arc::new(BlockingSuccessBarrier {
            calls: calls.clone(),
            entered: entered.clone(),
            release: release.clone(),
        }));
        let entered_wait = entered.notified();
        let first = tokio::spawn({
            let sender = sender.clone();
            async move { sender.commit_acceptance().await }
        });
        entered_wait.await;
        let second = tokio::spawn({
            let sender = sender.clone();
            async move { sender.commit_acceptance().await }
        });
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.add_permits(1);

        assert_eq!(first.await.unwrap(), Ok(()));
        assert_eq!(second.await.unwrap(), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn acceptance_error_is_cached_for_every_clone() {
        let calls = Arc::new(AtomicUsize::new(0));
        let sender = sender(Arc::new(CountingErrorBarrier(calls.clone())));

        let first = sender.commit_acceptance().await.unwrap_err();
        let second = sender.clone().commit_acceptance().await.unwrap_err();

        assert_eq!(first.code, "harness_outcome_unknown");
        assert_eq!(second, first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancelled_initializer_does_not_poison_a_later_acceptance_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let first_entered = Arc::new(Notify::new());
        let sender = sender(Arc::new(CancelThenSucceedBarrier {
            calls: calls.clone(),
            first_entered: first_entered.clone(),
            hold_first: Arc::new(Semaphore::new(0)),
        }));
        let entered_wait = first_entered.notified();
        let initializing = tokio::spawn({
            let sender = sender.clone();
            async move { sender.commit_acceptance().await }
        });
        entered_wait.await;
        initializing.abort();
        assert!(initializing.await.unwrap_err().is_cancelled());

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sender.commit_acceptance(),
        )
        .await
        .expect("a cancelled initializer releases the once-cell")
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
