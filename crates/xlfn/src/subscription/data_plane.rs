use super::delivery::{
    DeliveryPhase, NotificationAttempt, NotificationCompletion, QueuedUpdate, RefreshOutcome,
    RefreshPlan, RefreshState, RtdUpdate, SERVER_LIFECYCLE_OPEN, ShardRefreshBatch, SignalState,
    TopicShard, shard_index,
};
use super::host::SubscriptionHost;
use super::runtime_services::RuntimeServices;
use super::topic::{SubscriptionId, TopicId};
use super::value::StoredRtdValue;
use crate::generation::ConnectionGeneration;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use xlfn_kernel::operation_gate::{OperationGate, OperationGuard, TerminationWaitGuard};
use xlfn_kernel::quota::Quota;

pub(crate) struct PublishCore<H: SubscriptionHost> {
    host: H,
    runtime_gate: Arc<OperationGate>,
    server_gate: OperationGate,
    queued_update_quota: triomphe::Arc<Quota>,
    lifecycle: AtomicU8,
    publish_epoch: AtomicU64,
    next_update_sequence: AtomicU64,
    notified_epoch: AtomicU64,
    pending_updates: AtomicUsize,
    deliverable_pending: AtomicUsize,
    // Incremental work index only; shard.deliverable_count and pending maps are authoritative.
    ready_shards: AtomicU32,
    shards: Box<[Mutex<TopicShard>]>,
    refresh: Mutex<RefreshState<H::Notifier>>,
    services: Arc<RuntimeServices>,
}

impl<H: SubscriptionHost> PublishCore<H> {
    pub(crate) fn new(
        host: H,
        runtime_gate: Arc<OperationGate>,
        queued_update_quota: triomphe::Arc<Quota>,
        services: Arc<RuntimeServices>,
    ) -> Self {
        let mut shards = Vec::with_capacity(super::delivery::TOPIC_SHARDS);
        for _ in 0..super::delivery::TOPIC_SHARDS {
            shards.push(Mutex::new(TopicShard::default()));
        }

        Self {
            host,
            runtime_gate,
            server_gate: OperationGate::new(),
            queued_update_quota,
            lifecycle: AtomicU8::new(SERVER_LIFECYCLE_OPEN),
            publish_epoch: AtomicU64::new(0),
            next_update_sequence: AtomicU64::new(0),
            notified_epoch: AtomicU64::new(u64::MAX),
            pending_updates: AtomicUsize::new(0),
            deliverable_pending: AtomicUsize::new(0),
            ready_shards: AtomicU32::new(0),
            shards: shards.into_boxed_slice(),
            refresh: Mutex::new(RefreshState::default()),
            services,
        }
    }
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
            .field(
                "deliverable_pending",
                &self.deliverable_pending.load(Ordering::Relaxed),
            )
            .field("ready_shards", &self.ready_shards.load(Ordering::Relaxed))
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

pub(crate) struct PublishTerminationStart<'a, H: SubscriptionHost> {
    wait: TerminationWaitGuard<'a>,
    notifier: Option<H::Notifier>,
}

impl<H: SubscriptionHost> PublishTerminationStart<'_, H> {
    pub(crate) fn take_notifier(&mut self) -> Option<H::Notifier> {
        self.notifier.take()
    }

    pub(crate) fn wait(self) {
        self.wait.wait();
    }
}

pub(crate) struct RetiredConnection {
    pub(crate) id: SubscriptionId,
    pub(crate) generation: ConnectionGeneration,
}

pub(crate) struct PublishTerminationResult<N> {
    notifier: Option<N>,
    connections: Vec<RetiredConnection>,
}

impl<N> PublishTerminationResult<N> {
    pub(crate) fn into_parts(self) -> (Option<N>, Vec<RetiredConnection>) {
        (self.notifier, self.connections)
    }
}

pub(crate) struct InstalledConnection {
    pub(crate) latest: StoredRtdValue,
    pub(crate) observed_sequence: Option<u64>,
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

    fn clear_pending(&self) {
        for shard_mutex in self.shards.iter() {
            let mut shard = shard_mutex.lock();
            shard.pending[0].clear();
            shard.pending[1].clear();
            shard.deliverable_count = 0;
        }
        self.pending_updates.store(0, Ordering::Release);
        self.deliverable_pending.store(0, Ordering::Release);
        self.ready_shards.store(0, Ordering::Release);
    }

    pub(crate) fn begin_termination(&self) -> PublishTerminationStart<'_, H> {
        let wait = self.server_gate.close_and_wait_begin();
        self.lifecycle
            .store(super::delivery::SERVER_LIFECYCLE_CLOSING, Ordering::Release);
        let notifier = self.refresh.lock().detach_notifier();
        self.clear_pending();
        PublishTerminationStart { wait, notifier }
    }

    pub(crate) fn finish_termination(&self) -> PublishTerminationResult<H::Notifier> {
        let notifier = self.refresh.lock().detach_notifier();
        let mut connections = Vec::new();
        for shard_mutex in self.shards.iter() {
            let mut shard = shard_mutex.lock();
            shard.pending[0].clear();
            shard.pending[1].clear();
            connections.extend(shard.active_by_topic.drain().map(|(_, active)| {
                RetiredConnection {
                    id: active.id,
                    generation: active.generation,
                }
            }));
            shard.topic_by_id.clear();
        }
        self.pending_updates.store(0, Ordering::Release);
        self.deliverable_pending.store(0, Ordering::Release);
        self.ready_shards.store(0, Ordering::Release);
        self.lifecycle.store(
            super::delivery::SERVER_LIFECYCLE_TERMINATED,
            Ordering::Release,
        );
        PublishTerminationResult {
            notifier,
            connections,
        }
    }

    pub(crate) fn close_on_server_drop(&self) {
        self.lifecycle
            .store(super::delivery::SERVER_LIFECYCLE_CLOSING, Ordering::Release);
        self.clear_pending();
    }

    pub(crate) fn has_deliverable_updates(&self) -> bool {
        self.deliverable_pending.load(Ordering::Acquire) != 0
    }

    #[inline]
    fn allocate_update_sequence(&self) -> XllResult<u64> {
        self.next_update_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |sequence| {
                sequence.checked_add(1)
            })
            .map_err(|_| XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_OVERFLOW,
            })
    }

    #[inline]
    fn record_pending_insert(&self, shard: &mut TopicShard, shard_index: usize, deliverable: bool) {
        self.pending_updates.fetch_add(1, Ordering::Relaxed);
        if deliverable {
            self.record_deliverable_increase(shard, shard_index, 1);
        }
    }

    #[inline]
    fn record_pending_removal(
        &self,
        shard: &mut TopicShard,
        shard_index: usize,
        deliverable: bool,
    ) {
        let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.pending_updates);
        if deliverable {
            self.record_deliverable_decrease(shard, shard_index, 1);
        }
    }

    #[inline]
    fn record_deliverable_increase(
        &self,
        shard: &mut TopicShard,
        shard_index: usize,
        count: usize,
    ) {
        if count == 0 {
            return;
        }
        let was_empty = shard.deliverable_count == 0;
        shard.deliverable_count = shard
            .deliverable_count
            .checked_add(count)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        self.deliverable_pending.fetch_add(count, Ordering::Relaxed);
        if was_empty {
            self.ready_shards
                .fetch_or(1_u32 << shard_index, Ordering::Release);
        }
    }

    #[inline]
    fn record_deliverable_decrease(
        &self,
        shard: &mut TopicShard,
        shard_index: usize,
        count: usize,
    ) {
        if count == 0 {
            return;
        }
        shard.deliverable_count = shard
            .deliverable_count
            .checked_sub(count)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        for _ in 0..count {
            let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.deliverable_pending);
        }
        if shard.deliverable_count == 0 {
            self.ready_shards
                .fetch_and(!(1_u32 << shard_index), Ordering::Release);
        }
    }

    fn remove_pending_if(
        &self,
        shard: &mut TopicShard,
        shard_index: usize,
        buffer: usize,
        topic_id: TopicId,
        predicate: impl FnOnce(&QueuedUpdate) -> bool,
    ) -> bool {
        let deliverable = shard
            .active_by_topic
            .get(&topic_id)
            .is_some_and(|active| active.committed);
        let pending = &mut shard.pending[buffer];
        if !pending.get(&topic_id).is_some_and(predicate) {
            return false;
        }
        if pending.remove(&topic_id).is_none() {
            xlfn_kernel::invariant::fail_stop();
        }
        self.record_pending_removal(shard, shard_index, deliverable);
        true
    }

    fn retire_pending_through(
        &self,
        shard: &mut TopicShard,
        shard_index: usize,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        delivered_sequence: u64,
    ) {
        for buffer in [0, 1] {
            self.remove_pending_if(shard, shard_index, buffer, topic_id, |queued| {
                queued.connection_generation == generation && queued.sequence <= delivered_sequence
            });
        }
    }

    fn prepare_notification_for_known_update(
        &self,
        epoch: u64,
    ) -> XllResult<Option<NotificationAttempt<H::Notifier>>> {
        if self.notified_epoch.load(Ordering::Acquire) == epoch {
            return Ok(None);
        }
        let mut refresh = self.refresh.lock();
        let prepared = refresh.prepare_notification(true)?;
        Ok(prepared.map(|prepared| {
            self.notified_epoch.store(epoch, Ordering::Release);
            refresh.commit_notification(prepared)
        }))
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

    pub(crate) fn reserve_connection(
        &self,
        topic_id: TopicId,
        id: SubscriptionId,
        generation: ConnectionGeneration,
        active_quota: &triomphe::Arc<Quota>,
    ) -> XllResult<()> {
        let shard_index = shard_index(topic_id);
        let mut shard = self.shards[shard_index].lock();

        self.ensure_open()?;
        if shard.active_by_topic.contains_key(&topic_id) {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::TOPIC_ID_DUPLICATE,
            });
        }
        if let std::collections::hash_map::Entry::Vacant(topic_entry) = shard.topic_by_id.entry(id)
        {
            let permit = Quota::try_acquire(active_quota).map_err(|_| XllError::Overloaded)?;
            topic_entry.insert(topic_id);
            shard.active_by_topic.insert(
                topic_id,
                super::delivery::ActiveSubscription {
                    id,
                    generation,
                    committed: false,
                    latest: StoredRtdValue::Empty,
                    _permit: permit,
                },
            );
            Ok(())
        } else {
            Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::TOPIC_KEY_DUPLICATE,
            })
        }
    }

    pub(crate) fn install_connection(
        &self,
        topic_id: TopicId,
        generation: ConnectionGeneration,
    ) -> XllResult<InstalledConnection> {
        let shard_index = shard_index(topic_id);
        let mut shard = self.shards[shard_index].lock();
        self.ensure_open()?;
        let Some(active) = shard.active_by_topic.get_mut(&topic_id) else {
            return Err(XllError::Closing);
        };
        if active.generation != generation {
            return Err(XllError::Closing);
        }

        let latest = active.latest.clone();
        let epoch = self.publish_epoch.load(Ordering::Acquire);
        let buf0 = (epoch & 1) as usize;
        let buf1 = 1 - buf0;
        let observed_sequence = [buf0, buf1]
            .into_iter()
            .filter_map(|buffer| shard.pending[buffer].get(&topic_id))
            .filter(|update| update.connection_generation == generation)
            .map(|update| update.sequence)
            .max();

        Ok(InstalledConnection {
            latest,
            observed_sequence,
        })
    }

    pub(crate) fn commit_connection(
        &self,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        observed_sequence: Option<u64>,
    ) -> XllResult<Option<NotificationAttempt<H::Notifier>>> {
        let shard_index = shard_index(topic_id);
        let (epoch, has_pending) = {
            let mut shard = self.shards[shard_index].lock();
            self.ensure_open()?;
            let Some(active) = shard.active_by_topic.get(&topic_id) else {
                return Err(XllError::Closing);
            };
            if active.generation != generation {
                return Err(XllError::Closing);
            }
            let was_committed = active.committed;

            if let Some(obs) = observed_sequence {
                self.remove_pending_if(&mut shard, shard_index, 0, topic_id, |update| {
                    update.sequence <= obs
                });
                self.remove_pending_if(&mut shard, shard_index, 1, topic_id, |update| {
                    update.sequence <= obs
                });
            }

            let epoch = self.publish_epoch.load(Ordering::Acquire);
            let pending_count = shard
                .pending
                .iter()
                .filter(|pending| {
                    pending.get(&topic_id).is_some_and(|update| {
                        update.connection_generation == generation
                            && observed_sequence.is_none_or(|seq| update.sequence > seq)
                    })
                })
                .count();
            let active = shard
                .active_by_topic
                .get_mut(&topic_id)
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            active.committed = true;
            if !was_committed && pending_count != 0 {
                self.record_deliverable_increase(&mut shard, shard_index, pending_count);
            }
            let has_pending = pending_count != 0;
            (epoch, has_pending)
        };
        let attempt = if has_pending {
            self.prepare_notification_for_known_update(epoch)?
        } else {
            None
        };
        Ok(attempt)
    }

    pub(crate) fn rollback_connection(
        &self,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        id: SubscriptionId,
    ) {
        let shard_index = shard_index(topic_id);
        let mut shard = self.shards[shard_index].lock();

        self.remove_pending_if(&mut shard, shard_index, 0, topic_id, |update| {
            update.connection_generation == generation
        });
        self.remove_pending_if(&mut shard, shard_index, 1, topic_id, |update| {
            update.connection_generation == generation
        });

        if shard
            .active_by_topic
            .get(&topic_id)
            .is_some_and(|active| active.generation == generation)
        {
            shard.active_by_topic.remove(&topic_id);
        }
        if shard.topic_by_id.get(&id).is_some_and(|&tid| {
            shard
                .active_by_topic
                .get(&tid)
                .is_none_or(|active| active.generation == generation)
        }) {
            shard.topic_by_id.remove(&id);
        }
    }

    pub(crate) fn disconnect_connection(
        &self,
        topic_id: TopicId,
    ) -> XllResult<Option<RetiredConnection>> {
        let shard_index = shard_index(topic_id);
        let mut shard = self.shards[shard_index].lock();
        self.ensure_open()?;
        let Some(active) = shard.active_by_topic.get(&topic_id) else {
            return Ok(None);
        };
        let active_id = active.id;
        self.remove_pending_if(&mut shard, shard_index, 0, topic_id, |_| true);
        self.remove_pending_if(&mut shard, shard_index, 1, topic_id, |_| true);
        let Some((_tid, active)) = shard.active_by_topic.remove_entry(&topic_id) else {
            xlfn_kernel::invariant::fail_stop();
        };
        if active.id != active_id {
            xlfn_kernel::invariant::fail_stop();
        }
        shard.topic_by_id.remove(&active.id);
        Ok(Some(RetiredConnection {
            id: active.id,
            generation: active.generation,
        }))
    }

    pub(crate) fn publish(
        &self,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        value: StoredRtdValue,
    ) -> XllResult<()> {
        let _operation = self.enter_operation()?;

        let shard_index = shard_index(topic_id);

        let (epoch, committed) = loop {
            self.ensure_open()?;

            let epoch = self.publish_epoch.load(Ordering::Acquire);
            let buffer = (epoch & 1) as usize;

            let mut shard = self.shards[shard_index].lock();

            if self.publish_epoch.load(Ordering::Acquire) != epoch {
                drop(shard);
                continue;
            }

            let (committed, inserted) = {
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
                let committed = active.committed;
                let inserted = match pending_entry {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        let sequence = self.allocate_update_sequence()?;
                        let existing = entry.get_mut();
                        existing.connection_generation = conn_gen;
                        existing.sequence = sequence;
                        existing.value = value.clone();
                        false
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let permit = Quota::try_acquire(&self.queued_update_quota)
                            .map_err(|_| XllError::Overloaded)?;
                        let sequence = self.allocate_update_sequence()?;
                        entry.insert(QueuedUpdate {
                            connection_generation: conn_gen,
                            sequence,
                            value: value.clone(),
                            _permit: permit,
                        });
                        true
                    }
                };
                active.latest = value;
                (committed, inserted)
            };
            if inserted {
                self.record_pending_insert(&mut shard, shard_index, committed);
            }
            break (epoch, committed);
        };

        if committed && let Some(attempt) = self.prepare_notification_for_known_update(epoch)? {
            self.drive_notification(attempt);
        }
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
        delivered_updates: &[RtdUpdate],
        outcome: RefreshOutcome,
    ) -> XllResult<Option<NotificationAttempt<H::Notifier>>> {
        {
            let refresh = self.refresh.lock();
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
        }

        match outcome {
            RefreshOutcome::Delivered => {
                for update in delivered_updates {
                    let topic_id = TopicId(update.topic_id);
                    let shard_index = shard_index(topic_id);
                    let mut shard = self.shards[shard_index].lock();
                    self.retire_pending_through(
                        &mut shard,
                        shard_index,
                        topic_id,
                        update.connection_generation,
                        update.sequence,
                    );
                }
            }
            RefreshOutcome::Failed => {}
        }

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

    pub(crate) fn plan_refresh(&self) -> XllResult<PlannedRtdRefresh<'_, H>> {
        let operation = self.enter_operation()?;
        let plan = {
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

            let previous_epoch = self
                .publish_epoch
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                    epoch.checked_add(1)
                })
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::REFERENCE_OVERFLOW,
                })?;
            let epoch = previous_epoch + 1;
            refresh.phase = DeliveryPhase::Refreshing { refresh_id };
            RefreshPlan {
                refresh_id,
                epoch,
                candidate_shards: self.ready_shards.load(Ordering::Acquire),
            }
        };
        Ok(PlannedRtdRefresh {
            publish: self,
            operation: Some(operation),
            plan,
            finished: false,
        })
    }

    fn collect_shard(&self, shard_index: usize) -> Option<ShardRefreshBatch> {
        let shard = self.shards[shard_index].lock();
        let mut by_topic: FxHashMap<TopicId, (u64, ConnectionGeneration, StoredRtdValue)> =
            FxHashMap::default();
        by_topic.reserve(shard.deliverable_count);

        for pending in &shard.pending {
            for (topic_id, queued) in pending {
                let is_deliverable = shard.active_by_topic.get(topic_id).is_some_and(|active| {
                    active.committed && active.generation == queued.connection_generation
                });
                if !is_deliverable {
                    continue;
                }
                match by_topic.entry(*topic_id) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert((
                            queued.sequence,
                            queued.connection_generation,
                            queued.value.clone(),
                        ));
                    }
                    std::collections::hash_map::Entry::Occupied(mut slot) => {
                        if queued.sequence > slot.get().0 {
                            slot.insert((
                                queued.sequence,
                                queued.connection_generation,
                                queued.value.clone(),
                            ));
                        }
                    }
                }
            }
        }

        if by_topic.is_empty() {
            return None;
        }
        let updates = by_topic
            .into_iter()
            .map(
                |(topic_id, (sequence, connection_generation, value))| RtdUpdate {
                    sequence,
                    topic_id: topic_id.0,
                    connection_generation,
                    value,
                },
            )
            .collect();
        Some(ShardRefreshBatch {
            shard_index,
            updates,
        })
    }

    fn collect_refresh(&self, plan: &RefreshPlan) -> Vec<RtdUpdate> {
        debug_assert_eq!(self.publish_epoch.load(Ordering::Acquire), plan.epoch);
        let mut candidate_shards = plan.candidate_shards;
        let mut batches = Vec::with_capacity(candidate_shards.count_ones() as usize);
        while candidate_shards != 0 {
            let index = candidate_shards.trailing_zeros() as usize;
            candidate_shards &= candidate_shards - 1;
            if let Some(batch) = self.collect_shard(index) {
                batches.push(batch);
            }
        }
        reduce_refresh_batches(batches)
    }

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch<'_, H>> {
        Ok(self.plan_refresh()?.collect())
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        self.deliverable_pending.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn queued_update_count(&self) -> usize {
        self.pending_updates.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn lock_shard_for_test(
        &self,
        index: usize,
    ) -> parking_lot::MutexGuard<'_, TopicShard> {
        self.shards[index].lock()
    }

    #[cfg(test)]
    pub(crate) fn mark_closing_for_test(&self) {
        self.lifecycle
            .store(super::delivery::SERVER_LIFECYCLE_CLOSING, Ordering::Release);
    }
}

#[must_use]
pub(crate) struct PlannedRtdRefresh<'a, H: SubscriptionHost> {
    pub(crate) publish: &'a PublishCore<H>,
    pub(crate) operation: Option<ScopedPublishOperation<'a, H>>,
    pub(crate) plan: RefreshPlan,
    pub(crate) finished: bool,
}

impl<'a, H: SubscriptionHost> PlannedRtdRefresh<'a, H> {
    pub(crate) fn collect(self) -> RtdRefreshBatch<'a, H> {
        let updates = self.publish.collect_refresh(&self.plan);
        RtdRefreshBatch {
            transaction: self,
            updates,
        }
    }
}

impl<H: SubscriptionHost> Drop for PlannedRtdRefresh<'_, H> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.publish.abort_refresh_no_unwind(self.plan.refresh_id);
    }
}

#[must_use]
pub(crate) struct RtdRefreshBatch<'a, H: SubscriptionHost> {
    transaction: PlannedRtdRefresh<'a, H>,
    pub(crate) updates: Vec<RtdUpdate>,
}

impl<H: SubscriptionHost> RtdRefreshBatch<'_, H> {
    pub(crate) fn complete(mut self, outcome: RefreshOutcome) -> XllResult<()> {
        let attempt = self.transaction.publish.complete_refresh_inner(
            self.transaction.plan.refresh_id,
            &self.updates,
            outcome,
        )?;
        self.transaction.finished = true;
        if let Some(attempt) = attempt {
            self.transaction.publish.drive_notification(attempt);
        }
        self.transaction.operation.take();
        Ok(())
    }
}

fn reduce_refresh_batches(batches: Vec<ShardRefreshBatch>) -> Vec<RtdUpdate> {
    let update_count = batches.iter().map(|batch| batch.updates.len()).sum();
    let mut updates = Vec::with_capacity(update_count);
    for batch in batches {
        debug_assert!(
            batch
                .updates
                .iter()
                .all(|update| { shard_index(TopicId(update.topic_id)) == batch.shard_index })
        );
        updates.extend(batch.updates);
    }
    updates.sort_unstable_by_key(|update| update.sequence);
    updates
}
