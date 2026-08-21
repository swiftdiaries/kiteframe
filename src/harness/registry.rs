use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    HarnessApprovalBroker, HarnessEventEnvelope, HarnessEventSink, HarnessFailure,
    HarnessInvocationEvent, HarnessInvocationId, HarnessInvocationRequest, HarnessPlugin,
    HarnessRequestEnvelope, HarnessTerminal,
};

#[derive(Clone)]
pub struct HarnessRegistry {
    plugins: Arc<Mutex<BTreeMap<super::HarnessProfileId, RegisteredPlugin>>>,
    active: Arc<Mutex<BTreeMap<super::HarnessInvocationId, CancellationToken>>>,
}

#[derive(Clone)]
struct RegisteredPlugin {
    digest: String,
    plugin: Arc<dyn HarnessPlugin>,
}

impl Default for HarnessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Arc::new(Mutex::new(BTreeMap::new())),
            active: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub async fn register(&mut self, plugin: Arc<dyn HarnessPlugin>) -> Result<(), HarnessFailure> {
        let profile = plugin.profile();
        let capabilities = plugin.describe_capabilities().await?;
        profile.validate_observed(&capabilities)?;
        let digest = profile.digest()?;
        let mut plugins = self
            .plugins
            .lock()
            .expect("harness registry lock is not poisoned");
        if let Some(existing) = plugins.get(&profile.id) {
            if existing.digest == digest {
                return Ok(());
            }
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness profile registration conflicts with the active digest",
            ));
        }
        plugins.insert(profile.id.clone(), RegisteredPlugin { digest, plugin });
        Ok(())
    }

    pub fn active_count(&self) -> usize {
        self.active
            .lock()
            .expect("harness registry lock is not poisoned")
            .len()
    }

    pub async fn cancel(&self, invocation_id: &HarnessInvocationId) -> bool {
        let token = self
            .active
            .lock()
            .expect("harness registry lock is not poisoned")
            .get(invocation_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub async fn invoke(
        &self,
        request: HarnessInvocationRequest,
        approvals: Arc<dyn HarnessApprovalBroker>,
        sink: Arc<dyn HarnessEventSink>,
    ) -> HarnessTerminal {
        let Some(plugin) = self
            .plugins
            .lock()
            .expect("harness registry lock is not poisoned")
            .get(&request.profile_id)
            .map(|registered| registered.plugin.clone())
        else {
            return HarnessTerminal::Failed(HarnessFailure::new(
                "harness_profile_unavailable",
                "selected harness profile is not registered",
            ));
        };
        let envelope = HarnessRequestEnvelope::new(request.clone());
        if let Err(error) = envelope.validate() {
            return HarnessTerminal::Failed(error);
        }
        let token = CancellationToken::new();
        self.active
            .lock()
            .expect("harness registry lock is not poisoned")
            .insert(request.invocation_id.clone(), token.clone());
        let (events_tx, mut events_rx) = mpsc::channel(64);
        let invocation = plugin.start_invocation(envelope, events_tx, approvals, token);
        tokio::pin!(invocation);
        let mut sequence = 0;
        let terminal = loop {
            tokio::select! {
                terminal = &mut invocation => break terminal,
                Some(pending) = events_rx.recv() => {
                    match append_event(&sink, &request.invocation_id, sequence, pending.event).await {
                        Ok(()) => {
                            let _ = pending.persisted.send(Ok(()));
                            sequence += 1;
                        }
                        Err(error) => {
                            let _ = pending.persisted.send(Err(error.clone()));
                            break HarnessTerminal::OutcomeUnknown(error);
                        }
                    }
                }
            }
        };
        while let Ok(pending) = events_rx.try_recv() {
            match append_event(&sink, &request.invocation_id, sequence, pending.event).await {
                Ok(()) => {
                    let _ = pending.persisted.send(Ok(()));
                    sequence += 1;
                }
                Err(error) => {
                    let _ = pending.persisted.send(Err(error.clone()));
                    self.remove_active(&request.invocation_id);
                    return HarnessTerminal::OutcomeUnknown(error);
                }
            }
        }
        self.remove_active(&request.invocation_id);
        terminal
    }

    fn remove_active(&self, invocation_id: &HarnessInvocationId) {
        self.active
            .lock()
            .expect("harness registry lock is not poisoned")
            .remove(invocation_id);
    }
}

async fn append_event(
    sink: &Arc<dyn HarnessEventSink>,
    invocation_id: &HarnessInvocationId,
    sequence: u64,
    event: HarnessInvocationEvent,
) -> Result<(), HarnessFailure> {
    let envelope = HarnessEventEnvelope::new(invocation_id.clone(), sequence, event);
    envelope.validate()?;
    sink.append(envelope).await
}
