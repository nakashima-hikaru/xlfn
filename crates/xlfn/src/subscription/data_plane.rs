use super::delivery::{
    DeliveryPhase, NotificationAttempt, NotificationCompletion, QueuedUpdate, RefreshOutcome,
    RefreshState, RtdUpdate, SERVER_LIFECYCLE_OPEN, SignalState, TopicShard, shard_index,
};
use super::host::SubscriptionHost;
use super::runtime_services::RuntimeServices;
use super::topic::TopicId;
use super::value::StoredRtdValue;
use crate::generation::ConnectionGeneration;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use xlfn_kernel::operation_gate::{OperationGate, OperationGuard};
use xlfn_kernel::quota::Quota;

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
    pub(crate) services: Arc<RuntimeServices>,
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

pub(crate) struct ScopedPublishOperation<'a, H: SubscriptionHost> {
    pub(crate) _gate_guard: OperationGuard<'a>,
    pub(crate) _host_guard: H::AdmissionGuard,
    pub(crate) observation: ServerOperationObservation,
}

pub(crate) struct OwnedPublishOperation<H: SubscriptionHost> {
    core: triomphe::Arc<PublishCore<H>>,
    _host_guard: H::AdmissionGuard,
    _observation: ServerOperationObservation,
}

impl<H: SubscriptionHost> Drop for OwnedPublishOperation<H> {
    fn drop(&mut self) {
        self.core.server_gate.release();
    }
}

pub(crate) struct ServerOperationObservation {
    services: Arc<RuntimeServices>,
}

impl ServerOperationObservation {
    fn begin(services: &Arc<RuntimeServices>) -> Self {
        services.record(crate::shutdown_trace::ShutdownEvent::BeginRtdOperation);
        Self {
            services: Arc::clone(services),
        }
    }
}

impl Drop for ServerOperationObservation {
    fn drop(&mut self) {
        self.services
            .record(crate::shutdown_trace::ShutdownEvent::EndRtdOperation);
    }
}

struct ServerCallbackObservation {
    services: Arc<RuntimeServices>,
}

impl ServerCallbackObservation {
    fn begin(services: &Arc<RuntimeServices>) -> Self {
        services.record(crate::shutdown_trace::ShutdownEvent::BeginCallback);
        Self {
            services: Arc::clone(services),
        }
    }
}

impl Drop for ServerCallbackObservation {
    fn drop(&mut self) {
        self.services
            .record(crate::shutdown_trace::ShutdownEvent::EndCallback);
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

    pub(crate) fn enter_operation(&self) -> XllResult<ScopedPublishOperation<'_, H>> {
        if self.runtime_gate.is_closing() {
            return Err(XllError::Closing);
        }

        let mut gate_guard = None;
        let host_guard = self.host.enter_with(|| {
            gate_guard = Some(self.server_gate.enter().map_err(|_| XllError::Closing)?);
            Ok(())
        })?;

        Ok(ScopedPublishOperation {
            _gate_guard: gate_guard.expect("host admission acquires the server gate"),
            _host_guard: host_guard,
            observation: ServerOperationObservation::begin(&self.services),
        })
    }

    pub(crate) fn enter_owned_operation(
        core: triomphe::Arc<Self>,
    ) -> XllResult<OwnedPublishOperation<H>> {
        if core.runtime_gate.is_closing() {
            return Err(XllError::Closing);
        }

        let host_guard = core.host.enter_with(|| {
            core.server_gate.acquire().map_err(|_| XllError::Closing)?;
            Ok(())
        })?;
        let observation = ServerOperationObservation::begin(&core.services);

        Ok(OwnedPublishOperation {
            core,
            _host_guard: host_guard,
            _observation: observation,
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
            let _callback = ServerCallbackObservation::begin(&self.services);
            let res = catch_unwind(AssertUnwindSafe(|| self.host.notify(&attempt.notifier)));
            let completion = match res {
                Ok(Ok(())) => self.finish_notification_attempt(attempt.ticket, Ok(())),
                Ok(Err(err)) => self.finish_notification_attempt(attempt.ticket, Err(err)),
                Err(panic_payload) => {
                    let err = XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::PANIC_NOTIFY,
                    };
                    self.services.record_cleanup_result(Err(err.clone()));
                    std::panic::resume_unwind(panic_payload);
                }
            };

            match completion {
                NotificationCompletion::Finished => break,
                NotificationCompletion::Retry(next) => attempt = next,
                NotificationCompletion::Failed(err) => {
                    self.services.record_cleanup_result(Err(err));
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
                diagnostic_id: crate::diagnostics::id::DiagnosticId::NO_REFERENCE,
            });
        };

        if active_id != refresh_id {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_ID_MISMATCH,
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
                        let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.pending_updates);
                    }
                    if shard.pending[1]
                        .get(&topic_id)
                        .is_some_and(|u| u.sequence == update.sequence)
                    {
                        shard.pending[1].remove(&topic_id);
                        let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.pending_updates);
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

    pub(crate) fn attach_update_notifier(
        &self,
        notifier: H::Notifier,
    ) -> XllResult<Option<H::Notifier>> {
        let _operation = self.enter_operation()?;
        let (retired, attempt) = {
            self.ensure_open()?;
            let mut refresh = self.refresh.lock();
            let retired = refresh.attach_notifier(notifier);
            let has_updates = self.has_deliverable_updates();
            let prepared = refresh.prepare_notification(has_updates)?;
            let epoch = self.publish_epoch.load(Ordering::Acquire);
            let attempt = prepared.map(|p| {
                self.notified_epoch.store(epoch, Ordering::Release);
                refresh.commit_notification(p)
            });
            (retired, attempt)
        };
        if let Some(attempt) = attempt {
            self.drive_notification(attempt);
        }
        Ok(retired)
    }

    pub(crate) fn detach_update_notifier(&self) -> Option<H::Notifier> {
        let mut refresh = self.refresh.lock();
        refresh.detach_notifier()
    }

    pub(crate) fn pulse_notification(&self) -> XllResult<()> {
        let _operation = self.enter_operation()?;
        let attempt = {
            self.ensure_open()?;
            let mut refresh = self.refresh.lock();
            let has_updates = self.has_deliverable_updates();
            let prepared = refresh.prepare_notification(has_updates)?;
            let epoch = self.publish_epoch.load(Ordering::Acquire);
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

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch<'_, H>> {
        let operation = self.enter_operation()?;
        let (refresh_id, updates) = {
            self.ensure_open()?;
            let mut refresh = self.refresh.lock();
            if matches!(refresh.phase, DeliveryPhase::Refreshing { .. }) {
                return Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::OVERLAPPED_REFERENCE,
                });
            }
            let refresh_id = refresh.next_refresh_id;
            refresh.next_refresh_id = refresh_id.checked_add(1).ok_or(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_OVERFLOW,
            })?;

            self.publish_epoch.fetch_add(1, Ordering::AcqRel);

            let mut by_topic: FxHashMap<i32, (u64, StoredRtdValue)> = FxHashMap::default();
            for shard_mutex in self.shards.iter() {
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
            publish: self,
            operation: Some(operation),
            refresh_id,
            updates,
            completed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        let epoch = self.publish_epoch.load(Ordering::Acquire);
        let buf0 = (epoch & 1) as usize;
        let buf1 = 1 - buf0;
        let mut count = 0;
        for shard_mutex in self.shards.iter() {
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
}

#[must_use]
pub(crate) struct RtdRefreshBatch<'a, H: SubscriptionHost> {
    pub(crate) publish: &'a PublishCore<H>,
    pub(crate) operation: Option<ScopedPublishOperation<'a, H>>,
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
