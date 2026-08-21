use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    HarnessAcceptanceBarrier, HarnessApprovalBroker, HarnessEventEnvelope, HarnessEventSender,
    HarnessEventSink, HarnessFailure, HarnessInvocationId, HarnessInvocationRequest, HarnessPlugin,
    HarnessRequestEnvelope, HarnessTerminal, HarnessTerminalReporter, HarnessTransport,
};

const STABLE_FAILURE_CODES: [&str; 7] = [
    "harness_profile_unavailable",
    "harness_protocol_incompatible",
    "harness_capability_unsupported",
    "harness_permission_rejected",
    "harness_cancelled",
    "harness_failed",
    "harness_outcome_unknown",
];
const DEFAULT_MAX_PRE_ACCEPT_ATTEMPTS: u64 = 3;

/// Terminal-journal read boundary that must run before adapter work is admitted.
#[async_trait::async_trait]
pub trait HarnessTerminalJournal: Send + Sync {
    type Error: Send;

    async fn terminal(
        &self,
        invocation_id: &HarnessInvocationId,
    ) -> Result<Option<HarnessTerminal>, Self::Error>;
}

/// Run the replay branch when a durable terminal exists; only otherwise invoke the adapter branch.
///
/// The invocation branch owns settlement of a newly returned terminal before it completes.
pub async fn dispatch_with_terminal_journal<
    Journal,
    Replay,
    ReplayFuture,
    Invoke,
    InvokeFuture,
    Output,
>(
    journal: &Journal,
    invocation_id: &HarnessInvocationId,
    replay: Replay,
    invoke: Invoke,
) -> Result<Output, Journal::Error>
where
    Journal: HarnessTerminalJournal,
    Replay: FnOnce(HarnessTerminal) -> ReplayFuture,
    ReplayFuture: Future<Output = Result<Output, Journal::Error>>,
    Invoke: FnOnce() -> InvokeFuture,
    InvokeFuture: Future<Output = Result<Output, Journal::Error>>,
{
    if let Some(terminal) = journal.terminal(invocation_id).await? {
        return replay(terminal).await;
    }
    invoke().await
}

#[derive(Clone)]
pub struct HarnessRegistry {
    plugins: Arc<Mutex<BTreeMap<super::HarnessProfileId, RegisteredPlugin>>>,
    active: Arc<Mutex<ActiveInvocations>>,
}

#[derive(Clone)]
struct RegisteredPlugin {
    digest: String,
    plugin: Arc<dyn HarnessPlugin>,
    max_pre_accept_attempts: u64,
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

struct LifecycleTask(Option<tokio::task::JoinHandle<HarnessTerminal>>);

impl LifecycleTask {
    fn new(task: tokio::task::JoinHandle<HarnessTerminal>) -> Self {
        Self(Some(task))
    }

    fn task(&mut self) -> &mut tokio::task::JoinHandle<HarnessTerminal> {
        self.0.as_mut().expect("lifecycle task is owned")
    }

    fn take(&mut self) -> tokio::task::JoinHandle<HarnessTerminal> {
        self.0.take().expect("lifecycle task is owned")
    }

    async fn abort_and_wait(&mut self) {
        let Some(task) = self.0.take() else {
            return;
        };
        task.abort();
        let _ = task.await;
    }
}

impl Drop for LifecycleTask {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

pub(crate) struct ManagedHarnessInvocation {
    terminal: HarnessTerminal,
    lifecycle: Option<tokio::task::JoinHandle<HarnessTerminal>>,
    active_guard: Option<ActiveInvocationGuard>,
    started: bool,
    accepted: bool,
}

impl ManagedHarnessInvocation {
    fn settled(terminal: HarnessTerminal) -> Self {
        Self {
            terminal,
            lifecycle: None,
            active_guard: None,
            started: false,
            accepted: false,
        }
    }

    pub(crate) fn terminal(&self) -> &HarnessTerminal {
        &self.terminal
    }

    pub(crate) async fn finish(mut self) -> HarnessTerminal {
        let final_terminal = if let Some(lifecycle) = self.lifecycle.take() {
            match lifecycle.await {
                Ok(terminal) => validated_terminal(terminal, self.started, self.accepted),
                Err(error) => HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
                    "harness_outcome_unknown",
                    format!("harness lifecycle task failed: {error}"),
                )),
            }
        } else {
            self.terminal.clone()
        };
        self.active_guard.take();
        final_terminal
    }
}

impl Drop for ManagedHarnessInvocation {
    fn drop(&mut self) {
        if let Some(lifecycle) = self.lifecycle.take() {
            lifecycle.abort();
        }
    }
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
        self.register_with_pre_accept_attempt_limit(plugin, DEFAULT_MAX_PRE_ACCEPT_ATTEMPTS)
            .await
    }

    pub async fn register_with_pre_accept_attempt_limit(
        &mut self,
        plugin: Arc<dyn HarnessPlugin>,
        max_pre_accept_attempts: u64,
    ) -> Result<(), HarnessFailure> {
        if max_pre_accept_attempts == 0 {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "pre-accept attempt limit must be greater than zero",
            ));
        }
        let profile = plugin.profile();
        let capabilities = plugin
            .describe_capabilities()
            .await
            .map_err(|error| normalize_failure(error, "harness_failed"))?;
        profile.validate_observed(&capabilities)?;
        let digest = profile.digest()?;
        let mut plugins = self
            .plugins
            .lock()
            .expect("harness registry lock is not poisoned");
        if let Some(existing) = plugins.get(&profile.id) {
            if existing.digest == digest
                && existing.max_pre_accept_attempts == max_pre_accept_attempts
            {
                return Ok(());
            }
            return Err(HarnessFailure::new(
                "harness_protocol_incompatible",
                "harness profile registration conflicts with the active digest",
            ));
        }
        plugins.insert(
            profile.id.clone(),
            RegisteredPlugin {
                digest,
                plugin,
                max_pre_accept_attempts,
            },
        );
        Ok(())
    }

    pub fn active_count(&self) -> usize {
        self.active
            .lock()
            .expect("harness registry lock is not poisoned")
            .entries
            .len()
    }

    pub fn profile_digest(
        &self,
        profile_id: &super::HarnessProfileId,
    ) -> Result<String, HarnessFailure> {
        self.plugins
            .lock()
            .expect("harness registry lock is not poisoned")
            .get(profile_id)
            .map(|registered| registered.digest.clone())
            .ok_or_else(|| {
                HarnessFailure::new(
                    "harness_profile_unavailable",
                    "selected harness profile is not registered",
                )
            })
    }

    pub(crate) fn pre_accept_attempt_limit(
        &self,
        profile_id: &super::HarnessProfileId,
    ) -> Result<u64, HarnessFailure> {
        self.plugins
            .lock()
            .expect("harness registry lock is not poisoned")
            .get(profile_id)
            .map(|registered| registered.max_pre_accept_attempts)
            .ok_or_else(|| {
                HarnessFailure::new(
                    "harness_profile_unavailable",
                    "selected harness profile is not registered",
                )
            })
    }

    pub(crate) fn sole_profile_id_for_transport(
        &self,
        transport: HarnessTransport,
    ) -> Result<super::HarnessProfileId, HarnessFailure> {
        let plugins = self
            .plugins
            .lock()
            .expect("harness registry lock is not poisoned");
        let mut matching = plugins
            .values()
            .filter(|registered| registered.plugin.profile().transport == transport)
            .map(|registered| registered.plugin.profile().id.clone());
        let profile_id = matching.next().ok_or_else(|| {
            HarnessFailure::new(
                "harness_profile_unavailable",
                "no harness profile is registered for the selected transport",
            )
        })?;
        if matching.next().is_some() {
            return Err(HarnessFailure::new(
                "harness_profile_unavailable",
                "multiple harness profiles require an explicit deployment route",
            ));
        }
        Ok(profile_id)
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
        _request: HarnessInvocationRequest,
        _approvals: Arc<dyn HarnessApprovalBroker>,
        _sink: Arc<dyn HarnessEventSink>,
    ) -> HarnessTerminal {
        HarnessTerminal::Failed(HarnessFailure::new(
            "harness_profile_unavailable",
            "durable harness acceptance barrier is required",
        ))
    }

    pub async fn invoke_with_acceptance_barrier(
        &self,
        request: HarnessInvocationRequest,
        approvals: Arc<dyn HarnessApprovalBroker>,
        sink: Arc<dyn HarnessEventSink>,
        acceptance_barrier: Arc<dyn HarnessAcceptanceBarrier>,
    ) -> HarnessTerminal {
        self.invoke_from_sequence_with_acceptance_barrier(
            request,
            approvals,
            sink,
            0,
            acceptance_barrier,
        )
        .await
    }

    pub(crate) async fn invoke_from_sequence_with_acceptance_barrier(
        &self,
        request: HarnessInvocationRequest,
        approvals: Arc<dyn HarnessApprovalBroker>,
        sink: Arc<dyn HarnessEventSink>,
        next_sequence: u64,
        acceptance_barrier: Arc<dyn HarnessAcceptanceBarrier>,
    ) -> HarnessTerminal {
        self.invoke_managed_from_sequence_with_acceptance_barrier(
            request,
            approvals,
            sink,
            next_sequence,
            acceptance_barrier,
        )
        .await
        .finish()
        .await
    }

    pub(crate) async fn invoke_managed_from_sequence_with_acceptance_barrier(
        &self,
        request: HarnessInvocationRequest,
        approvals: Arc<dyn HarnessApprovalBroker>,
        sink: Arc<dyn HarnessEventSink>,
        next_sequence: u64,
        acceptance_barrier: Arc<dyn HarnessAcceptanceBarrier>,
    ) -> ManagedHarnessInvocation {
        let Some(plugin) = self
            .plugins
            .lock()
            .expect("harness registry lock is not poisoned")
            .get(&request.profile_id)
            .map(|registered| registered.plugin.clone())
        else {
            return ManagedHarnessInvocation::settled(HarnessTerminal::Failed(
                HarnessFailure::new(
                    "harness_profile_unavailable",
                    "selected harness profile is not registered",
                ),
            ));
        };
        let envelope = HarnessRequestEnvelope::new(request.clone());
        if let Err(error) = envelope.validate() {
            return ManagedHarnessInvocation::settled(HarnessTerminal::Failed(error));
        }
        let Some((token, active_guard)) = self.reserve_active(&request.invocation_id) else {
            return ManagedHarnessInvocation::settled(HarnessTerminal::Failed(
                HarnessFailure::new("harness_failed", "harness invocation id is already active"),
            ));
        };
        let (raw_events_tx, mut events_rx) = mpsc::channel(64);
        let events_tx = HarnessEventSender::new(raw_events_tx, acceptance_barrier);
        let (reporter, mut reported_terminal) = HarnessTerminalReporter::channel();
        let mut lifecycle = LifecycleTask::new(tokio::spawn(plugin.start_invocation_managed(
            envelope,
            events_tx.clone(),
            approvals,
            token,
            reporter,
        )));
        let mut sequence = next_sequence;
        let mut started = next_sequence > 0;
        let mut terminal = None;
        let mut persistence_failure = None;
        let mut lifecycle_finished = false;

        loop {
            tokio::select! {
                biased;
                result = &mut reported_terminal => match result {
                    Ok(reported) => {
                        terminal = Some(reported);
                        break;
                    }
                    Err(_) => {
                        terminal = Some(HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
                            "harness_outcome_unknown",
                            "harness lifecycle closed without reporting a terminal",
                        )));
                        break;
                    }
                },
                result = lifecycle.task() => {
                    terminal = Some(match result {
                        Ok(terminal) => terminal,
                        Err(error) => HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
                            "harness_outcome_unknown",
                            format!("harness lifecycle task failed: {error}"),
                        )),
                    });
                    lifecycle_finished = true;
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
            if !lifecycle_finished {
                lifecycle.abort_and_wait().await;
            }
            return ManagedHarnessInvocation {
                terminal: HarnessTerminal::OutcomeUnknown(error),
                lifecycle: None,
                active_guard: Some(active_guard),
                started,
                accepted: events_tx.acceptance_committed(),
            };
        }
        let accepted = events_tx.acceptance_committed();
        let terminal = validated_terminal(
            terminal.expect("plugin completion is required without a persistence failure"),
            started,
            accepted,
        );
        ManagedHarnessInvocation {
            terminal,
            lifecycle: (!lifecycle_finished).then(|| lifecycle.take()),
            active_guard: Some(active_guard),
            started,
            accepted,
        }
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

fn normalize_terminal(terminal: HarnessTerminal) -> HarnessTerminal {
    match terminal {
        HarnessTerminal::Retryable(error) => {
            HarnessTerminal::Retryable(normalize_failure(error, "harness_failed"))
        }
        HarnessTerminal::Failed(error) => {
            HarnessTerminal::Failed(normalize_failure(error, "harness_failed"))
        }
        HarnessTerminal::OutcomeUnknown(error) => {
            HarnessTerminal::OutcomeUnknown(normalize_failure(error, "harness_outcome_unknown"))
        }
        completed_or_cancelled => completed_or_cancelled,
    }
}

fn validated_terminal(
    terminal: HarnessTerminal,
    started: bool,
    acceptance_committed: bool,
) -> HarnessTerminal {
    let terminal = normalize_terminal(terminal);
    if matches!(terminal, HarnessTerminal::Completed(_)) && !started {
        return HarnessTerminal::Failed(HarnessFailure::new(
            "harness_protocol_incompatible",
            "harness invocation completed without a durable Started event",
        ));
    }
    if matches!(terminal, HarnessTerminal::Completed(_)) && !acceptance_committed {
        return HarnessTerminal::Failed(HarnessFailure::new(
            "harness_protocol_incompatible",
            "harness invocation completed without committing acceptance",
        ));
    }
    if matches!(terminal, HarnessTerminal::Retryable(_)) && acceptance_committed {
        return HarnessTerminal::OutcomeUnknown(HarnessFailure::new(
            "harness_outcome_unknown",
            "harness returned a retryable failure after accepting external work",
        ));
    }
    terminal
}

fn normalize_failure(error: HarnessFailure, fallback: &'static str) -> HarnessFailure {
    if STABLE_FAILURE_CODES.contains(&error.code) {
        error
    } else {
        HarnessFailure::new(fallback, error.detail)
    }
}

async fn append_pending(
    sink: &Arc<dyn HarnessEventSink>,
    invocation_id: &HarnessInvocationId,
    sequence: &mut u64,
    started: &mut bool,
    pending: super::HarnessPendingEvent,
) -> Result<(), HarnessFailure> {
    if *started && matches!(pending.event, super::HarnessInvocationEvent::Started) {
        let _ = pending.persisted.send(Ok(()));
        return Ok(());
    }
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
            let outcome_unknown = HarnessFailure::new("harness_outcome_unknown", error.detail);
            let _ = pending.persisted.send(Err(outcome_unknown.clone()));
            Err(outcome_unknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::harness::{
        HARNESS_PROTOCOL_VERSION, HarnessCapabilities, HarnessCapability, HarnessProfile,
        HarnessProfileId,
    };

    struct StaticPlugin {
        profile: HarnessProfile,
    }

    #[async_trait::async_trait]
    impl HarnessPlugin for StaticPlugin {
        fn profile(&self) -> &HarnessProfile {
            &self.profile
        }

        async fn describe_capabilities(&self) -> Result<HarnessCapabilities, HarnessFailure> {
            Ok(HarnessCapabilities {
                protocol_version: HARNESS_PROTOCOL_VERSION,
                implementation_name: "static-test-plugin".to_owned(),
                implementation_version: "1".to_owned(),
                supported: self.profile.required.clone(),
            })
        }

        async fn start_invocation(
            &self,
            _request: HarnessRequestEnvelope,
            _events: HarnessEventSender,
            _approvals: Arc<dyn HarnessApprovalBroker>,
            _cancellation: super::super::HarnessCancellation,
        ) -> HarnessTerminal {
            HarnessTerminal::Cancelled
        }
    }

    fn plugin(id: &str) -> Arc<dyn HarnessPlugin> {
        Arc::new(StaticPlugin {
            profile: HarnessProfile {
                id: HarnessProfileId::new(id).unwrap(),
                transport: HarnessTransport::AcpStdio,
                source_repository: "https://example.test/static.git".to_owned(),
                source_revision: "1111111111111111111111111111111111111111".to_owned(),
                program: "static-test-plugin".to_owned(),
                args: vec![],
                required: BTreeSet::from([HarnessCapability::FreshSession]),
                optional: BTreeSet::new(),
                fresh_sessions_only: true,
            },
        })
    }

    #[tokio::test]
    async fn registration_retains_profile_specific_pre_accept_attempt_limit() {
        let mut registry = HarnessRegistry::new();
        let custom_id = HarnessProfileId::new("custom-limit-v1").unwrap();
        registry
            .register_with_pre_accept_attempt_limit(plugin(custom_id.as_str()), 1)
            .await
            .unwrap();
        assert_eq!(registry.pre_accept_attempt_limit(&custom_id).unwrap(), 1);

        let default_id = HarnessProfileId::new("default-limit-v1").unwrap();
        registry
            .register(plugin(default_id.as_str()))
            .await
            .unwrap();
        assert_eq!(registry.pre_accept_attempt_limit(&default_id).unwrap(), 3);
    }

    #[tokio::test]
    async fn registration_rejects_zero_pre_accept_attempt_limit() {
        let mut registry = HarnessRegistry::new();
        let error = registry
            .register_with_pre_accept_attempt_limit(plugin("zero-limit-v1"), 0)
            .await
            .unwrap_err();
        assert_eq!(error.code, "harness_profile_unavailable");
    }
}
