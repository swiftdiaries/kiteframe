use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::Error as _};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{PostgresStore, canonical_sha256};

use super::{
    HARNESS_PROTOCOL_VERSION, HarnessAcceptanceBarrier, HarnessApprovalDecision,
    HarnessApprovalRequest, HarnessCancellation, HarnessEventSender, HarnessFailure,
    HarnessInvocationEvent, HarnessInvocationId, HarnessInvocationRequest, HarnessPendingEvent,
    HarnessProfile, HarnessProfileId, HarnessTerminal,
};

/// Internal terminal handoff used by lifecycle-owning plugins. Implementations
/// may report a successful child terminal before bounded process shutdown; the
/// returned `start_invocation_managed` future must still own that shutdown to
/// completion.
#[doc(hidden)]
#[derive(Clone)]
pub struct HarnessTerminalReporter(Arc<Mutex<Option<oneshot::Sender<HarnessTerminal>>>>);

impl HarnessTerminalReporter {
    pub(crate) fn channel() -> (Self, oneshot::Receiver<HarnessTerminal>) {
        let (sender, receiver) = oneshot::channel();
        (Self(Arc::new(Mutex::new(Some(sender)))), receiver)
    }

    pub fn report(&self, terminal: HarnessTerminal) {
        if let Some(sender) = self
            .0
            .lock()
            .expect("terminal reporter lock is not poisoned")
            .take()
        {
            let _ = sender.send(terminal);
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessRequestEnvelope {
    pub protocol_version: u16,
    pub profile_id: HarnessProfileId,
    pub invocation: HarnessInvocationRequest,
}

impl<'de> Deserialize<'de> for HarnessRequestEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RawHarnessRequestEnvelope {
            protocol_version: u16,
            profile_id: HarnessProfileId,
            invocation: HarnessInvocationRequest,
        }

        let raw = RawHarnessRequestEnvelope::deserialize(deserializer)?;
        let envelope = Self {
            protocol_version: raw.protocol_version,
            profile_id: raw.profile_id,
            invocation: raw.invocation,
        };
        envelope.validate().map_err(D::Error::custom)?;
        Ok(envelope)
    }
}

impl HarnessRequestEnvelope {
    pub fn new(invocation: HarnessInvocationRequest) -> Self {
        Self {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            profile_id: invocation.profile_id.clone(),
            invocation,
        }
    }

    pub fn validate(&self) -> Result<(), HarnessFailure> {
        if self.protocol_version != HARNESS_PROTOCOL_VERSION {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness request protocol version is unsupported",
            ));
        }
        if self.profile_id != self.invocation.profile_id {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness request profile does not match its invocation",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EventDigestInput<'a> {
    protocol_version: u16,
    invocation_id: &'a HarnessInvocationId,
    sequence: u64,
    event: &'a HarnessInvocationEvent,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessEventEnvelope {
    pub protocol_version: u16,
    pub invocation_id: HarnessInvocationId,
    pub sequence: u64,
    pub event_digest: String,
    pub event: HarnessInvocationEvent,
}

impl HarnessEventEnvelope {
    pub fn new(
        invocation_id: HarnessInvocationId,
        sequence: u64,
        event: HarnessInvocationEvent,
    ) -> Self {
        let event_digest = event_digest(HARNESS_PROTOCOL_VERSION, &invocation_id, sequence, &event)
            .expect("harness event envelope fields are canonical-JSON serializable");
        Self {
            protocol_version: HARNESS_PROTOCOL_VERSION,
            invocation_id,
            sequence,
            event_digest,
            event,
        }
    }

    pub fn validate(&self) -> Result<(), HarnessFailure> {
        if self.protocol_version != HARNESS_PROTOCOL_VERSION {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness event protocol version is unsupported",
            ));
        }
        if (self.sequence == 0) != matches!(self.event, HarnessInvocationEvent::Started) {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness event sequence zero must be Started",
            ));
        }
        let digest = event_digest(
            self.protocol_version,
            &self.invocation_id,
            self.sequence,
            &self.event,
        )
        .map_err(|error| HarnessFailure::new("harness_protocol_incompatible", error.to_string()))?;
        if self.event_digest != digest {
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness event digest does not match its canonical envelope",
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for HarnessEventEnvelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct RawHarnessEventEnvelope {
            protocol_version: u16,
            invocation_id: HarnessInvocationId,
            sequence: u64,
            event_digest: String,
            event: HarnessInvocationEvent,
        }

        let raw = RawHarnessEventEnvelope::deserialize(deserializer)?;
        let envelope = Self {
            protocol_version: raw.protocol_version,
            invocation_id: raw.invocation_id,
            sequence: raw.sequence,
            event_digest: raw.event_digest,
            event: raw.event,
        };
        envelope.validate().map_err(D::Error::custom)?;
        Ok(envelope)
    }
}

fn event_digest(
    protocol_version: u16,
    invocation_id: &HarnessInvocationId,
    sequence: u64,
    event: &HarnessInvocationEvent,
) -> Result<String, crate::HashError> {
    canonical_sha256(&EventDigestInput {
        protocol_version,
        invocation_id,
        sequence,
        event,
    })
}

#[async_trait]
pub trait HarnessApprovalBroker: Send + Sync {
    async fn decide(&self, request: HarnessApprovalRequest) -> HarnessApprovalDecision;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllHarnessApprovals;

#[async_trait]
impl HarnessApprovalBroker for DenyAllHarnessApprovals {
    async fn decide(&self, _request: HarnessApprovalRequest) -> HarnessApprovalDecision {
        HarnessApprovalDecision::RejectOnce
    }
}

#[async_trait]
pub trait HarnessEventSink: Send + Sync {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure>;
}

#[async_trait]
pub trait HarnessPlugin: Send + Sync {
    fn profile(&self) -> &HarnessProfile;

    async fn describe_capabilities(&self) -> Result<super::HarnessCapabilities, HarnessFailure>;

    async fn start_invocation(
        &self,
        request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        approvals: Arc<dyn HarnessApprovalBroker>,
        cancellation: HarnessCancellation,
    ) -> HarnessTerminal;

    #[doc(hidden)]
    async fn start_invocation_managed(
        self: Arc<Self>,
        request: HarnessRequestEnvelope,
        events: HarnessEventSender,
        approvals: Arc<dyn HarnessApprovalBroker>,
        cancellation: HarnessCancellation,
        reporter: HarnessTerminalReporter,
    ) -> HarnessTerminal {
        let terminal = self
            .start_invocation(request, events, approvals, cancellation)
            .await;
        reporter.report(terminal.clone());
        terminal
    }
}

pub async fn emit_and_wait(
    events: &HarnessEventSender,
    event: HarnessInvocationEvent,
) -> Result<(), HarnessFailure> {
    let (persisted, acknowledgement) = oneshot::channel();
    events
        .send(HarnessPendingEvent { event, persisted })
        .await
        .map_err(|_| {
            HarnessFailure::new("harness_protocol_incompatible", "event receiver closed")
        })?;
    acknowledgement.await.map_err(|_| {
        HarnessFailure::new(
            "harness_protocol_incompatible",
            "event persistence acknowledgement closed",
        )
    })?
}

#[derive(Clone)]
pub struct PostgresHarnessEventSink {
    store: Arc<PostgresStore>,
}

impl PostgresHarnessEventSink {
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self { store }
    }
}

struct PostgresHarnessAcceptanceBarrier {
    store: Arc<PostgresStore>,
    invocation_id: HarnessInvocationId,
    expected_attempt: u64,
}

#[async_trait]
impl HarnessAcceptanceBarrier for PostgresHarnessAcceptanceBarrier {
    async fn commit(&self) -> Result<(), HarnessFailure> {
        let invocation = self
            .store
            .mark_harness_prompt_accepted(&self.invocation_id, self.expected_attempt)
            .await
            .map_err(|error| HarnessFailure::new("harness_outcome_unknown", error.to_string()))?;
        if invocation.state == crate::store::HarnessInvocationState::Cancelled {
            return Err(HarnessFailure::new(
                "harness_cancelled",
                "harness command was cancelled before prompt acceptance",
            ));
        }
        Ok(())
    }
}

pub(crate) fn postgres_harness_acceptance_barrier(
    store: Arc<PostgresStore>,
    invocation_id: HarnessInvocationId,
    expected_attempt: u64,
) -> Arc<dyn HarnessAcceptanceBarrier> {
    Arc::new(PostgresHarnessAcceptanceBarrier {
        store,
        invocation_id,
        expected_attempt,
    })
}

#[async_trait]
impl HarnessEventSink for PostgresHarnessEventSink {
    async fn append(&self, event: HarnessEventEnvelope) -> Result<(), HarnessFailure> {
        self.store
            .append_harness_event_envelope(&event)
            .await
            .map_err(|error| HarnessFailure::new("harness_outcome_unknown", error.to_string()))
    }
}

#[allow(dead_code)]
fn _cancellation_token_is_exported(_: CancellationToken) {}
