use super::catalog::{SubscriptionCatalog, SubscriptionState};
use super::delivery::{
    DeliveryPhase, NotificationAttempt, NotificationCompletion, QueuedUpdate, RefreshOutcome,
    RefreshState, RtdUpdate, SERVER_LIFECYCLE_CLOSING, SERVER_LIFECYCLE_OPEN,
    SERVER_LIFECYCLE_TERMINATED, SignalState, TopicShard, shard_index,
};
use super::host::SubscriptionHost;
use super::runtime::{SubscriptionConnection, SubscriptionRuntime};
use super::source::{ErasedRtdSource, RtdSubscription};
use super::topic::{SubscriptionKey, TopicId};
use super::value::StoredRtdValue;
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use rustc_hash::FxHashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use xlfn_kernel::operation_gate::{OperationGate, OperationGuard, TerminationWaitGuard};
use xlfn_kernel::quota::Quota;

#[derive(Clone)]
pub(crate) struct SubscriptionServerHandle<H: SubscriptionHost> {
    pub(crate) inner: Arc<SubscriptionServer<H>>,
}

impl<H: SubscriptionHost> SubscriptionServerHandle<H> {
    pub(crate) fn attach_update_notifier(
        &self,
        notifier: H::Notifier,
    ) -> XllResult<Option<H::Notifier>> {
        self.inner.attach_update_notifier(notifier)
    }

    pub(crate) fn detach_update_notifier(&self) -> Option<H::Notifier> {
        self.inner.detach_update_notifier()
    }

    pub(crate) fn pulse_notification(&self) -> XllResult<()> {
        self.inner.pulse_notification()
    }

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch<'_, H>> {
        self.inner.begin_refresh()
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        self.inner.pending_update_count()
    }

    pub(crate) fn claim(&self, key: &SubscriptionKey) -> XllResult<()> {
        let _operation = self.inner.enter_operation()?;
        self.inner.ensure_open()?;
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.claim_server_key(self.inner.generation, key)
    }

    pub(crate) fn connect_transaction(
        &self,
        topic_id: TopicId,
        key: &SubscriptionKey,
    ) -> XllResult<SubscriptionConnection<H>> {
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.connect_transaction(self, topic_id, key)
    }

    pub(crate) fn disconnect(&self, topic_id: TopicId) -> XllResult<()> {
        let _operation = self.inner.enter_operation()?;
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.disconnect(self, topic_id)
    }

    pub(crate) fn terminate(&self) -> XllResult<()> {
        self.inner.terminate()
    }
}

pub(crate) struct PublishCore<H: SubscriptionHost> {
    pub(crate) host: H,
    pub(crate) runtime_gate: Arc<OperationGate>,
    pub(crate) server_gate: OperationGate,
    pub(crate) queued_update_quota: triomphe::Arc<Quota>,
    pub(crate) lifecycle: AtomicU8,
    pub(crate) publish_epoch: AtomicU64,
    pub(crate) next_update_sequence: AtomicU64,
    pub(crate) notified_epoch: AtomicU64,
    pub(crate) pending_updates: AtomicUsize,
    pub(crate) shards: Box<[Mutex<TopicShard>]>,
    pub(crate) refresh: Mutex<RefreshState<H::Notifier>>,
    pub(crate) parent: Weak<SubscriptionRuntime<H>>,
}

impl<H: SubscriptionHost> std::fmt::Debug for PublishCore<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishCore")
            .field("lifecycle", &self.lifecycle.load(Ordering::Relaxed))
            .field("publish_epoch", &self.publish_epoch.load(Ordering::Relaxed))
            .field(
                "next_update_sequence",
                &self.next_update_sequence.load(Ordering::Relaxed),
            )
            .field(
                "notified_epoch",
                &self.notified_epoch.load(Ordering::Relaxed),
            )
            .field(
                "pending_updates",
                &self.pending_updates.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

pub(crate) struct SubscriptionServer<H: SubscriptionHost> {
    pub(crate) generation: ServerGeneration,
    pub(crate) publish: triomphe::Arc<PublishCore<H>>,
    pub(crate) subscriptions: Mutex<FxHashMap<TopicId, Box<dyn RtdSubscription>>>,
    pub(crate) parent: Weak<SubscriptionRuntime<H>>,
    pub(crate) termination_coordinator: TerminationCoordinator,
}

impl<H: SubscriptionHost> std::fmt::Debug for SubscriptionServer<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionServer")
            .field("generation", &self.generation)
            .field("publish", &self.publish)
            .finish_non_exhaustive()
    }
}

pub(crate) struct ScopedServerOperation<'a, H: SubscriptionHost> {
    pub(crate) _gate_guard: OperationGuard<'a>,
    pub(crate) _host_guard: H::AdmissionGuard,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) parent: Weak<SubscriptionRuntime<H>>,
}

#[cfg(any(test, feature = "refinement"))]
impl<H: SubscriptionHost> Drop for ScopedServerOperation<'_, H> {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.upgrade() {
            parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
    }
}

pub(crate) struct OwnedServerOperation<H: SubscriptionHost> {
    pub(crate) server: Arc<SubscriptionServer<H>>,
    pub(crate) _host_guard: H::AdmissionGuard,
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) parent: Weak<SubscriptionRuntime<H>>,
}

impl<H: SubscriptionHost> Drop for OwnedServerOperation<H> {
    fn drop(&mut self) {
        self.server.publish.server_gate.release();
        #[cfg(any(test, feature = "refinement"))]
        if let Some(parent) = self.parent.upgrade() {
            parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
    }
}

impl<H: SubscriptionHost> PublishCore<H> {
    #[inline]
    pub(crate) fn ensure_open(&self) -> XllResult<()> {
        if self.lifecycle.load(Ordering::Acquire) == SERVER_LIFECYCLE_OPEN {
            Ok(())
        } else {
            Err(XllError::Closing)
        }
    }

    pub(crate) fn has_deliverable_updates(&self) -> bool {
        let epoch = self.publish_epoch.load(Ordering::Acquire);
        let buf0 = (epoch & 1) as usize;
        let buf1 = 1 - buf0;
        self.shards.iter().any(|shard_mutex| {
            let shard = shard_mutex.lock();
            shard.pending[buf0]
                .keys()
                .chain(shard.pending[buf1].keys())
                .any(|tid| {
                    shard
                        .active_by_topic
                        .get(tid)
                        .is_some_and(|active| active.committed)
                })
        })
    }

    pub(crate) fn ensure_notified(&self, epoch: u64) -> XllResult<()> {
        if self.notified_epoch.load(Ordering::Acquire) == epoch {
            return Ok(());
        }
        let attempt = {
            let mut refresh = self.refresh.lock();
            let has_updates = self.has_deliverable_updates();
            let prepared = refresh.prepare_notification(has_updates)?;
            prepared.map(|p| {
                self.notified_epoch.store(epoch, Ordering::Release);
                refresh.commit_notification(p)
            })
        };
        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
        Ok(())
    }

    pub(crate) fn enter_operation(&self) -> XllResult<ScopedServerOperation<'_, H>> {
        if self.runtime_gate.is_closing() {
            return Err(XllError::Closing);
        }

        let mut gate_guard = None;
        let host_guard = self.host.enter_with(|| {
            gate_guard = Some(self.server_gate.enter().map_err(|_| XllError::Closing)?);
            #[cfg(any(test, feature = "refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
            }
            Ok(())
        })?;

        Ok(ScopedServerOperation {
            _gate_guard: gate_guard.expect("host admission acquires the server gate"),
            _host_guard: host_guard,
            #[cfg(any(test, feature = "refinement"))]
            parent: std::sync::Weak::clone(&self.parent),
        })
    }

    pub(crate) fn enter_owned_operation(
        &self,
        server: Arc<SubscriptionServer<H>>,
    ) -> XllResult<OwnedServerOperation<H>> {
        if self.runtime_gate.is_closing() {
            return Err(XllError::Closing);
        }

        let host_guard = self.host.enter_with(|| {
            self.server_gate.acquire().map_err(|_| XllError::Closing)?;
            #[cfg(any(test, feature = "refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
            }
            Ok(())
        })?;

        Ok(OwnedServerOperation {
            server,
            _host_guard: host_guard,
            #[cfg(any(test, feature = "refinement"))]
            parent: std::sync::Weak::clone(&self.parent),
        })
    }

    pub(crate) fn publish(
        &self,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        value: StoredRtdValue,
    ) -> XllResult<()> {
        let _operation = self.enter_operation()?;

        let shard_index = shard_index(topic_id);

        let epoch = loop {
            self.ensure_open()?;

            let epoch = self.publish_epoch.load(Ordering::Acquire);
            let buffer = (epoch & 1) as usize;

            let mut shard = self.shards[shard_index].lock();

            if self.publish_epoch.load(Ordering::Acquire) != epoch {
                drop(shard);
                continue;
            }

            let TopicShard {
                active_by_topic,
                pending: pending_buffers,
                ..
            } = &mut *shard;
            let pending = &mut pending_buffers[buffer];
            let pending_entry = pending.entry(topic_id);
            let active = active_by_topic
                .get_mut(&topic_id)
                .filter(|active| active.generation == generation)
                .ok_or(XllError::Closing)?;
            let conn_gen = active.generation;
            match pending_entry {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let sequence = self.next_update_sequence.fetch_add(1, Ordering::Relaxed);
                    let existing = entry.get_mut();
                    existing.connection_generation = conn_gen;
                    existing.sequence = sequence;
                    existing.value = value.clone();
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let permit = Quota::try_acquire(&self.queued_update_quota)
                        .map_err(|_| XllError::Overloaded)?;
                    let sequence = self.next_update_sequence.fetch_add(1, Ordering::Relaxed);
                    entry.insert(QueuedUpdate {
                        connection_generation: conn_gen,
                        sequence,
                        value: value.clone(),
                        _permit: permit,
                    });
                    self.pending_updates.fetch_add(1, Ordering::Relaxed);
                }
            }

            active.latest = value;
            break epoch;
        };

        self.ensure_notified(epoch)?;
        Ok(())
    }

    pub(crate) fn drive_notification(&self, mut attempt: NotificationAttempt<H::Notifier>) {
        loop {
            #[cfg(any(test, feature = "refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginCallback);
            }
            let res = catch_unwind(AssertUnwindSafe(|| self.host.notify(&attempt.notifier)));
            #[cfg(any(test, feature = "refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::EndCallback);
            }
            let completion = match res {
                Ok(Ok(())) => self.finish_notification_attempt(attempt.ticket, Ok(())),
                Ok(Err(err)) => self.finish_notification_attempt(attempt.ticket, Err(err)),
                Err(panic_payload) => {
                    let err = XllError::Internal {
                        diagnostic_id: crate::error::DiagnosticId::PANIC_NOTIFY,
                    };
                    if let Some(parent) = self.parent.upgrade() {
                        parent.record_cleanup_result(Err(err.clone()));
                    }
                    std::panic::resume_unwind(panic_payload);
                }
            };

            match completion {
                NotificationCompletion::Finished => break,
                NotificationCompletion::Retry(next) => attempt = next,
                NotificationCompletion::Failed(err) => {
                    if let Some(parent) = self.parent.upgrade() {
                        parent.record_cleanup_result(Err(err));
                    }
                    break;
                }
            }
        }
    }

    pub(crate) fn finish_notification_attempt(
        &self,
        ticket: u64,
        outcome: XllResult<()>,
    ) -> NotificationCompletion<H::Notifier> {
        let mut refresh = self.refresh.lock();
        let notifier = refresh.notifier.clone();
        let Some(signal) = refresh.signal_for_ticket_mut(ticket) else {
            return NotificationCompletion::Finished;
        };

        let attempt = match signal {
            SignalState::Calling { ticket: t, attempt } if *t == ticket => *attempt,
            _ => return NotificationCompletion::Finished,
        };

        match outcome {
            Ok(()) => {
                if let Some(signal) = refresh.signal_calling_mut(ticket) {
                    *signal = SignalState::Signaled { ticket };
                }
                NotificationCompletion::Finished
            }
            Err(error) => {
                if attempt < 2 {
                    let next_attempt = attempt + 1;
                    if let Some(signal) = refresh.signal_calling_mut(ticket) {
                        *signal = SignalState::Calling {
                            ticket,
                            attempt: next_attempt,
                        };
                    }
                    if let Some(notifier) = notifier {
                        NotificationCompletion::Retry(NotificationAttempt { ticket, notifier })
                    } else {
                        if let DeliveryPhase::BetweenRefreshes {
                            signal: signal @ SignalState::Suppressed { .. },
                        } = &mut refresh.phase
                        {
                            *signal = SignalState::Dormant;
                        }
                        NotificationCompletion::Failed(error)
                    }
                } else {
                    if let Some(signal) = refresh.signal_calling_mut(ticket) {
                        *signal = SignalState::Suppressed { ticket };
                    }
                    NotificationCompletion::Failed(error)
                }
            }
        }
    }

    pub(crate) fn complete_refresh_inner(
        &self,
        refresh_id: u64,
        _delivered_updates: &[RtdUpdate],
        outcome: RefreshOutcome,
    ) -> XllResult<Option<NotificationAttempt<H::Notifier>>> {
        let mut refresh = self.refresh.lock();
        let DeliveryPhase::Refreshing {
            refresh_id: active_id,
        } = refresh.phase
        else {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::NO_REFERENCE,
            });
        };

        if active_id != refresh_id {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::REFERENCE_ID_MISMATCH,
            });
        }

        refresh.ensure_notification_ticket()?;

        match outcome {
            RefreshOutcome::Delivered => {
                for update in _delivered_updates {
                    let topic_id = TopicId(update.topic_id);
                    let shard_index = shard_index(topic_id);
                    let mut shard = self.shards[shard_index].lock();
                    if shard.pending[0]
                        .get(&topic_id)
                        .is_some_and(|u| u.sequence == update.sequence)
                    {
                        shard.pending[0].remove(&topic_id);
                        self.pending_updates.fetch_sub(1, Ordering::Relaxed);
                    }
                    if shard.pending[1]
                        .get(&topic_id)
                        .is_some_and(|u| u.sequence == update.sequence)
                    {
                        shard.pending[1].remove(&topic_id);
                        self.pending_updates.fetch_sub(1, Ordering::Relaxed);
                    }
                }
            }
            RefreshOutcome::Failed => {}
        }

        refresh.phase = DeliveryPhase::BetweenRefreshes {
            signal: SignalState::Dormant,
        };

        let has_updates = self.has_deliverable_updates();
        let prepared = refresh.prepare_notification(has_updates)?;
        let attempt = prepared.map(|p| refresh.commit_notification(p));
        Ok(attempt)
    }

    pub(crate) fn abort_refresh_no_unwind(&self, refresh_id: u64) {
        let attempt = {
            let mut refresh = self.refresh.lock();
            if let DeliveryPhase::Refreshing {
                refresh_id: active_id,
            } = refresh.phase
            {
                if active_id == refresh_id {
                    refresh.phase = DeliveryPhase::BetweenRefreshes {
                        signal: SignalState::Dormant,
                    };
                    let has_updates = self.has_deliverable_updates();
                    let prepared = refresh.prepare_notification(has_updates).ok().flatten();
                    prepared.map(|p| refresh.commit_notification(p))
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
    }
}

impl<H: SubscriptionHost> SubscriptionServer<H> {
    #[inline]
    pub(crate) fn ensure_open(&self) -> XllResult<()> {
        self.publish.ensure_open()
    }

    #[inline]
    pub(crate) fn enter_operation(&self) -> XllResult<ScopedServerOperation<'_, H>> {
        self.publish.enter_operation()
    }

    #[inline]
    pub(crate) fn enter_owned_operation(self: &Arc<Self>) -> XllResult<OwnedServerOperation<H>> {
        self.publish.enter_owned_operation(Arc::clone(self))
    }

    pub(crate) fn attach_update_notifier(
        &self,
        notifier: H::Notifier,
    ) -> XllResult<Option<H::Notifier>> {
        let _operation = self.publish.enter_operation()?;
        let (retired, attempt) = {
            self.publish.ensure_open()?;
            let mut refresh = self.publish.refresh.lock();
            let retired = refresh.attach_notifier(notifier);
            let has_updates = self.publish.has_deliverable_updates();
            let prepared = refresh.prepare_notification(has_updates)?;
            let epoch = self.publish.publish_epoch.load(Ordering::Acquire);
            let attempt = prepared.map(|p| {
                self.publish.notified_epoch.store(epoch, Ordering::Release);
                refresh.commit_notification(p)
            });
            (retired, attempt)
        };
        if let Some(attempt) = attempt {
            self.publish.drive_notification(attempt);
        }
        Ok(retired)
    }

    pub(crate) fn detach_update_notifier(&self) -> Option<H::Notifier> {
        let mut refresh = self.publish.refresh.lock();
        refresh.detach_notifier()
    }

    pub(crate) fn pulse_notification(&self) -> XllResult<()> {
        let _operation = self.publish.enter_operation()?;
        let attempt = {
            self.publish.ensure_open()?;
            let mut refresh = self.publish.refresh.lock();
            let has_updates = self.publish.has_deliverable_updates();
            let prepared = refresh.prepare_notification(has_updates)?;
            let epoch = self.publish.publish_epoch.load(Ordering::Acquire);
            prepared.map(|p| {
                self.publish.notified_epoch.store(epoch, Ordering::Release);
                refresh.commit_notification(p)
            })
        };
        if let Some(attempt) = attempt {
            self.publish.drive_notification(attempt);
        }
        Ok(())
    }

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch<'_, H>> {
        let operation = self.enter_operation()?;
        let (refresh_id, updates) = {
            self.publish.ensure_open()?;
            let mut refresh = self.publish.refresh.lock();
            if matches!(refresh.phase, DeliveryPhase::Refreshing { .. }) {
                return Err(XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::OVERLAPPED_REFERENCE,
                });
            }
            let refresh_id = refresh.next_refresh_id;
            refresh.next_refresh_id = refresh_id.checked_add(1).ok_or(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::REFERENCE_OVERFLOW,
            })?;

            self.publish.publish_epoch.fetch_add(1, Ordering::AcqRel);

            let mut by_topic: FxHashMap<i32, (u64, StoredRtdValue)> = FxHashMap::default();
            for shard_mutex in self.publish.shards.iter() {
                let shard = shard_mutex.lock();
                for buf in [0, 1] {
                    for (topic_id, queued) in &shard.pending[buf] {
                        if shard
                            .active_by_topic
                            .get(topic_id)
                            .is_some_and(|active| active.committed)
                        {
                            match by_topic.entry(topic_id.0) {
                                std::collections::hash_map::Entry::Vacant(slot) => {
                                    slot.insert((queued.sequence, queued.value.clone()));
                                }
                                std::collections::hash_map::Entry::Occupied(mut slot) => {
                                    if queued.sequence > slot.get().0 {
                                        slot.insert((queued.sequence, queued.value.clone()));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let mut updates_vec: Vec<RtdUpdate> = by_topic
                .into_iter()
                .map(|(topic_id, (sequence, value))| RtdUpdate {
                    sequence,
                    topic_id,
                    value,
                })
                .collect();
            updates_vec.sort_unstable_by_key(|u| u.sequence);

            refresh.phase = DeliveryPhase::Refreshing { refresh_id };
            (refresh_id, updates_vec)
        };
        Ok(RtdRefreshBatch {
            publish: self.publish.as_ref(),
            operation: Some(operation),
            refresh_id,
            updates,
            completed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        let epoch = self.publish.publish_epoch.load(Ordering::Acquire);
        let buf0 = (epoch & 1) as usize;
        let buf1 = 1 - buf0;
        let mut count = 0;
        for shard_mutex in self.publish.shards.iter() {
            let shard = shard_mutex.lock();
            let keys0 = shard.pending[buf0].keys();
            let keys1 = shard.pending[buf1].keys();
            for topic_id in keys0.chain(keys1) {
                if shard
                    .active_by_topic
                    .get(topic_id)
                    .is_some_and(|a| a.committed)
                {
                    count += 1;
                }
            }
        }
        count
    }

    pub(crate) fn remove_from_registry(&self) {
        if let Some(parent) = self.parent.upgrade() {
            let mut servers = parent.servers.lock();
            servers.remove(&self.generation);
        }
    }

    pub(crate) fn begin_termination<'a>(self: &'a Arc<Self>) -> TerminationAdmission<'a, H> {
        let mut term_state = self.termination_coordinator.state.lock();
        match term_state.phase {
            ServerTerminationPhase::Terminated | ServerTerminationPhase::Failed => {
                TerminationAdmission::Complete
            }
            ServerTerminationPhase::Terminating => {
                TerminationAdmission::Waiter(ServerTerminationWaiter {
                    coordinator: &self.termination_coordinator,
                })
            }
            ServerTerminationPhase::Open => {
                let wait = self.publish.server_gate.close_and_wait_begin();
                term_state.phase = ServerTerminationPhase::Terminating;

                self.publish
                    .lifecycle
                    .store(SERVER_LIFECYCLE_CLOSING, Ordering::Release);

                let notifier = {
                    let mut refresh = self.publish.refresh.lock();
                    refresh.detach_notifier()
                };

                for shard_mutex in self.publish.shards.iter() {
                    let mut shard = shard_mutex.lock();
                    shard.pending[0].clear();
                    shard.pending[1].clear();
                }
                self.publish.pending_updates.store(0, Ordering::Release);

                let initial_subscriptions: Vec<_> = self
                    .subscriptions
                    .lock()
                    .drain()
                    .map(|(_, sub)| sub)
                    .collect();

                TerminationAdmission::Owner(ServerTermination {
                    server: Arc::clone(self),
                    wait,
                    notifier,
                    initial_subscriptions,
                })
            }
        }
    }

    pub(crate) fn termination_result(&self) -> XllResult<()> {
        let state = self.termination_coordinator.state.lock();
        state
            .failure
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    pub(crate) fn terminate(self: &Arc<Self>) -> XllResult<()> {
        match self.begin_termination() {
            TerminationAdmission::Owner(owner) => {
                let res = owner.request_cancel();
                owner.finish(res)
            }
            TerminationAdmission::Waiter(waiter) => waiter.wait(),
            TerminationAdmission::Complete => self.termination_result(),
        }
    }
}

impl<H: SubscriptionHost> Drop for SubscriptionServer<H> {
    fn drop(&mut self) {
        self.publish
            .lifecycle
            .store(SERVER_LIFECYCLE_CLOSING, Ordering::Release);
        for shard_mutex in self.publish.shards.iter() {
            let mut shard = shard_mutex.lock();
            shard.pending[0].clear();
            shard.pending[1].clear();
        }
        self.publish.pending_updates.store(0, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ServerTerminationPhase {
    #[default]
    Open,
    Terminating,
    Terminated,
    Failed,
}

#[derive(Debug, Default)]
pub(crate) struct TerminationState {
    pub(crate) phase: ServerTerminationPhase,
    pub(crate) failure: Option<XllError>,
}

pub(crate) struct TerminationCoordinator {
    pub(crate) state: Mutex<TerminationState>,
    pub(crate) completed: Condvar,
}

impl Default for TerminationCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(TerminationState::default()),
            completed: Condvar::new(),
        }
    }
}

pub(crate) enum TerminationAdmission<'a, H: SubscriptionHost> {
    Owner(ServerTermination<'a, H>),
    Waiter(ServerTerminationWaiter<'a>),
    Complete,
}

pub(crate) struct ServerTerminationWaiter<'a> {
    pub(crate) coordinator: &'a TerminationCoordinator,
}

impl<'a> ServerTerminationWaiter<'a> {
    pub(crate) fn wait(self) -> XllResult<()> {
        let mut state = self.coordinator.state.lock();
        while state.phase == ServerTerminationPhase::Terminating {
            self.coordinator.completed.wait(&mut state);
        }
        match state.phase {
            ServerTerminationPhase::Terminated | ServerTerminationPhase::Failed => state
                .failure
                .as_ref()
                .map_or(Ok(()), |error| Err(error.clone())),
            _ => unreachable!(),
        }
    }
}

pub(crate) struct TerminationCompletionGuard<'a> {
    pub(crate) coordinator: &'a TerminationCoordinator,
    pub(crate) failure: Option<XllError>,
    pub(crate) completed: bool,
}

impl TerminationCompletionGuard<'_> {
    pub(crate) fn complete(mut self, result: XllResult<()>) -> XllResult<()> {
        self.failure = result.as_ref().err().cloned();
        self.publish_completion(ServerTerminationPhase::Terminated);
        self.completed = true;
        result
    }

    pub(crate) fn publish_completion(&self, phase: ServerTerminationPhase) {
        let mut state = self.coordinator.state.lock();
        state.failure = self.failure.clone();
        state.phase = phase;
        self.coordinator.completed.notify_all();
    }
}

impl Drop for TerminationCompletionGuard<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }

        if self.failure.is_none() {
            self.failure = Some(XllError::Panic);
        }

        self.publish_completion(ServerTerminationPhase::Failed);
    }
}

#[allow(
    clippy::drop_non_drop,
    reason = "RtdNotifier contains drop types on Windows/test configurations but may be uninhabited on non-Windows production"
)]
pub(crate) fn drop_notifier_no_unwind<N>(notifier: Option<N>) -> XllResult<()> {
    catch_unwind(AssertUnwindSafe(|| drop(notifier))).map_err(|_| XllError::Panic)
}

pub(crate) struct TerminatedTopic {
    pub(crate) key: SubscriptionKey,
    pub(crate) generation: ConnectionGeneration,
}

thread_local! {
    pub(crate) static PANIC_AFTER_TERMINATION_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) struct ServerTermination<'a, H: SubscriptionHost> {
    pub(crate) server: Arc<SubscriptionServer<H>>,
    pub(crate) wait: TerminationWaitGuard<'a>,
    pub(crate) notifier: Option<H::Notifier>,
    pub(crate) initial_subscriptions: Vec<Box<dyn RtdSubscription>>,
}

impl<'a, H: SubscriptionHost> ServerTermination<'a, H> {
    pub(crate) fn request_cancel(&self) -> XllResult<()> {
        let mut first_error = None;
        for sub in &self.initial_subscriptions {
            if catch_unwind(AssertUnwindSafe(|| sub.cancellation().request_cancel())).is_err()
                && first_error.is_none()
            {
                first_error = Some(XllError::Panic);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn finish(mut self, cancel_result: XllResult<()>) -> XllResult<()> {
        let guard = TerminationCompletionGuard {
            coordinator: &self.server.termination_coordinator,
            failure: None,
            completed: false,
        };

        #[cfg(test)]
        if PANIC_AFTER_TERMINATION_GUARD.replace(false) {
            panic!("injected termination owner panic");
        }

        let mut first_error = cancel_result.err();

        if let Err(err) = drop_notifier_no_unwind(self.notifier.take())
            && first_error.is_none()
        {
            first_error = Some(err);
        }

        let wait_res = catch_unwind(AssertUnwindSafe(|| self.wait.wait()));
        if wait_res.is_err() && first_error.is_none() {
            first_error = Some(XllError::Panic);
        }

        let (late_notifier, active_entries) = {
            let late_notifier = self.server.publish.refresh.lock().detach_notifier();
            let mut active_entries = Vec::new();
            for shard_mutex in self.server.publish.shards.iter() {
                let mut shard = shard_mutex.lock();
                shard.pending[0].clear();
                shard.pending[1].clear();
                for (_, active) in shard.active_by_topic.drain() {
                    active_entries.push(TerminatedTopic {
                        key: active.key,
                        generation: active.generation,
                    });
                }
                shard.topic_by_key.clear();
            }
            self.server
                .publish
                .pending_updates
                .store(0, Ordering::Release);
            self.server
                .publish
                .lifecycle
                .store(SERVER_LIFECYCLE_TERMINATED, Ordering::Release);

            (late_notifier, active_entries)
        };

        #[cfg(any(test, feature = "refinement"))]
        if let Some(parent) = self.server.parent.upgrade() {
            for _ in 0..self.initial_subscriptions.len() {
                parent
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveSubscription);
            }
        }

        if let Err(err) = drop_notifier_no_unwind(late_notifier)
            && first_error.is_none()
        {
            first_error = Some(err);
        }

        let removed_sources = if let Some(parent) = self.server.parent.upgrade() {
            let mut catalog = parent.catalog.lock();
            let mut sources = Vec::new();

            for topic in &active_entries {
                if let Some(src) = cleanup_catalog_binding_and_pending(
                    &mut catalog,
                    &topic.key,
                    self.server.generation,
                    topic.generation,
                ) {
                    sources.push(src);
                }
            }

            sources
        } else {
            Vec::new()
        };

        if let Some(parent) = self.server.parent.upgrade() {
            let mut catalog = parent.catalog.lock();
            let unactive_pending_keys: Vec<_> = catalog
                .entries
                .iter()
                .filter(|(_, entry)| {
                    entry.state != SubscriptionState::Active
                        && entry.server_generation == Some(self.server.generation)
                })
                .map(|(k, _)| *k)
                .collect();

            let mut extra_sources = Vec::new();
            for key in unactive_pending_keys {
                let Some(should_remove) = catalog.with_entry(&key, |entry| {
                    entry.server_generation = None;
                    entry.connection_generation = None;
                    entry.state = SubscriptionState::Pending;
                    entry.committed = false;
                    entry.can_remove()
                }) else {
                    continue;
                };

                if should_remove
                    && let Some(removed) = catalog.remove_entry(&key)
                    && let Some(source) = removed.source
                {
                    extra_sources.push(source);
                }
            }
            drop(catalog);
            for src in extra_sources {
                if catch_unwind(AssertUnwindSafe(|| drop(src))).is_err() && first_error.is_none() {
                    first_error = Some(XllError::Panic);
                }
            }
        }

        for source in removed_sources {
            if catch_unwind(AssertUnwindSafe(|| drop(source))).is_err() && first_error.is_none() {
                first_error = Some(XllError::Panic);
            }
        }

        let all_subscriptions: Vec<Box<dyn RtdSubscription>> = self
            .initial_subscriptions
            .drain(..)
            .chain(self.server.subscriptions.lock().drain().map(|(_, s)| s))
            .collect();

        if let Err(error) = disconnect_all_no_unwind(all_subscriptions)
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        let result = first_error.map_or(Ok(()), Err);

        if let Some(parent) = self.server.parent.upgrade() {
            parent.record_cleanup_result(result.clone());
        }

        self.server.remove_from_registry();

        guard.complete(result)
    }
}

#[must_use]
pub(crate) struct RtdRefreshBatch<'a, H: SubscriptionHost> {
    pub(crate) publish: &'a PublishCore<H>,
    pub(crate) operation: Option<ScopedServerOperation<'a, H>>,
    pub(crate) refresh_id: u64,
    pub(crate) updates: Vec<RtdUpdate>,
    pub(crate) completed: bool,
}

impl<H: SubscriptionHost> RtdRefreshBatch<'_, H> {
    pub(crate) fn complete(mut self, outcome: RefreshOutcome) -> XllResult<()> {
        let attempt =
            self.publish
                .complete_refresh_inner(self.refresh_id, &self.updates, outcome)?;
        if let Some(attempt) = attempt {
            self.publish.drive_notification(attempt);
        }
        self.completed = true;
        self.operation.take();
        Ok(())
    }
}

impl<H: SubscriptionHost> Drop for RtdRefreshBatch<'_, H> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.publish.abort_refresh_no_unwind(self.refresh_id);
    }
}

pub(crate) fn disconnect_one_no_unwind(subscription: Box<dyn RtdSubscription>) -> XllResult<()> {
    match catch_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait())) {
        Ok(result) => result,
        Err(_) => Err(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::PANIC_DISCONNECT,
        }),
    }
}

pub(crate) fn disconnect_all_no_unwind(
    subscriptions: impl IntoIterator<Item = Box<dyn RtdSubscription>>,
) -> XllResult<()> {
    let mut first_error = None;
    for subscription in subscriptions {
        if let Err(err) = disconnect_one_no_unwind(subscription)
            && first_error.is_none()
        {
            first_error = Some(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(crate) fn cleanup_catalog_binding_and_pending(
    catalog: &mut SubscriptionCatalog,
    key: &SubscriptionKey,
    server_generation: ServerGeneration,
    conn_generation: ConnectionGeneration,
) -> Option<Arc<dyn ErasedRtdSource>> {
    let (_, should_remove) = catalog.with_entry(key, |entry| {
        if entry.connection_generation != Some(conn_generation)
            || entry.server_generation != Some(server_generation)
        {
            return (false, false);
        }

        entry.state = SubscriptionState::Pending;
        entry.connection_generation = None;
        entry.server_generation = None;
        entry.committed = false;
        (true, entry.can_remove())
    })?;

    if should_remove {
        return catalog.remove_entry(key).and_then(|entry| entry.source);
    }

    None
}

pub(crate) enum ServerReservationFailure {
    DuplicateTopicId,
    DuplicateKey,
    Overloaded(XllError),
}

impl ServerReservationFailure {
    pub(crate) fn into_xll_error(self) -> XllError {
        match self {
            ServerReservationFailure::DuplicateTopicId => XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::TOPIC_ID_DUPLICATE,
            },
            ServerReservationFailure::DuplicateKey => XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::TOPIC_KEY_DUPLICATE,
            },
            ServerReservationFailure::Overloaded(err) => err,
        }
    }
}
