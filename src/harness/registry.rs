use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    HarnessApprovalBroker, HarnessEventEnvelope, HarnessEventSender, HarnessEventSink,
    HarnessFailure, HarnessInvocationId, HarnessInvocationRequest, HarnessPlugin,
    HarnessRequestEnvelope, HarnessTerminal,
};

#[derive(Clone)]
pub struct HarnessRegistry {
    plugins: Arc<Mutex<BTreeMap<super::HarnessProfileId, RegisteredPlugin>>>,
    active: Arc<Mutex<ActiveInvocations>>,
}

#[derive(Clone)]
struct RegisteredPlugin {
    digest: String,
    plugin: Arc<dyn HarnessPlugin>,
}

struct ActiveInvocations {
    next_generation: u64,
    entries: BTreeMap<HarnessInvocationId, ActiveInvocation>,
}

#[derive(Clone)]
struct ActiveInvocation {
    generation: u64,
    token: CancellationToken,
}

struct ActiveInvocationGuard {
    active: Arc<Mutex<ActiveInvocations>>,
    invocation_id: HarnessInvocationId,
    generation: u64,
}

impl Drop for ActiveInvocationGuard {
    fn drop(&mut self) {
        let mut active = self
            .active
            .lock()
            .expect("harness registry lock is not poisoned");
        if active
            .entries
            .get(&self.invocation_id)
            .is_some_and(|entry| entry.generation == self.generation)
        {
            active.entries.remove(&self.invocation_id);
        }
    }
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
            active: Arc::new(Mutex::new(ActiveInvocations {
                next_generation: 0,
                entries: BTreeMap::new(),
            })),
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
            .entries
            .len()
    }

    pub async fn cancel(&self, invocation_id: &HarnessInvocationId) -> bool {
        let token = self
            .active
            .lock()
            .expect("harness registry lock is not poisoned")
            .entries
            .get(invocation_id)
            .map(|entry| entry.token.clone());
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
        let Some((token, _active_guard)) = self.reserve_active(&request.invocation_id) else {
            return HarnessTerminal::Failed(HarnessFailure::new(
                "harness_invocation_active",
                "harness invocation id is already active",
            ));
        };
        let (raw_events_tx, mut events_rx) = mpsc::channel(64);
        let events_tx = HarnessEventSender::new(raw_events_tx);
        let invocation = plugin.start_invocation(envelope, events_tx.clone(), approvals, token);
        tokio::pin!(invocation);
        let mut sequence = 0;
        let mut started = false;
        let mut terminal = None;
        let mut persistence_failure = None;

        loop {
            tokio::select! {
                result = &mut invocation => {
                    terminal = Some(result);
                    break;
                }
                Some(pending) = events_rx.recv() => {
                    if let Err(error) = append_pending(
                        &sink,
                        &request.invocation_id,
                        &mut sequence,
                        &mut started,
                        pending,
                    ).await {
                        persistence_failure = Some(error);
                        break;
                    }
                }
            }
        }

        events_tx.close_admission();
        events_rx.close();
        while let Some(pending) = events_rx.recv().await {
            if let Some(error) = persistence_failure.as_ref() {
                let _ = pending.persisted.send(Err(error.clone()));
                continue;
            }
            if let Err(error) = append_pending(
                &sink,
                &request.invocation_id,
                &mut sequence,
                &mut started,
                pending,
            )
            .await
            {
                persistence_failure = Some(error);
            }
        }

        if let Some(error) = persistence_failure {
            return HarnessTerminal::OutcomeUnknown(error);
        }
        let terminal =
            terminal.expect("plugin completion is required without a persistence failure");
        if matches!(terminal, HarnessTerminal::Completed(_)) && !started {
            return HarnessTerminal::Failed(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness invocation completed without a durable Started event",
            ));
        }
        terminal
    }

    fn reserve_active(
        &self,
        invocation_id: &HarnessInvocationId,
    ) -> Option<(CancellationToken, ActiveInvocationGuard)> {
        let mut active = self
            .active
            .lock()
            .expect("harness registry lock is not poisoned");
        if active.entries.contains_key(invocation_id) {
            return None;
        }
        active.next_generation = active.next_generation.wrapping_add(1);
        let generation = active.next_generation;
        let token = CancellationToken::new();
        active.entries.insert(
            invocation_id.clone(),
            ActiveInvocation {
                generation,
                token: token.clone(),
            },
        );
        Some((
            token,
            ActiveInvocationGuard {
                active: self.active.clone(),
                invocation_id: invocation_id.clone(),
                generation,
            },
        ))
    }
}

async fn append_pending(
    sink: &Arc<dyn HarnessEventSink>,
    invocation_id: &HarnessInvocationId,
    sequence: &mut u64,
    started: &mut bool,
    pending: super::HarnessPendingEvent,
) -> Result<(), HarnessFailure> {
    let envelope = HarnessEventEnvelope::new(invocation_id.clone(), *sequence, pending.event);
    if let Err(error) = envelope.validate() {
        let _ = pending.persisted.send(Err(error.clone()));
        return Err(error);
    }
    match sink.append(envelope).await {
        Ok(()) => {
            if *sequence == 0 {
                *started = true;
            }
            *sequence += 1;
            let _ = pending.persisted.send(Ok(()));
            Ok(())
        }
        Err(error) => {
            let _ = pending.persisted.send(Err(error.clone()));
            Err(error)
        }
    }
}
