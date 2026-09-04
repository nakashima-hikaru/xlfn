#![allow(
    unsafe_code,
    reason = "publish permits borrow runtime-owned quotas proven live by RTD shutdown"
)]

use super::delivery::{
    DeliveryPhase, NotificationAttempt, NotificationCompletion, QueuedUpdate, RefreshOutcome,
    RefreshPlan, RefreshState, RtdUpdate, SERVER_LIFECYCLE_OPEN, ShardRefreshBatch, SignalState,
    TopicShard, ValueSlot, VersionedRtdValue, shard_index,
};
use super::host::SubscriptionHost;
use super::runtime_services::RuntimeServices;
use super::topic::{SubscriptionId, TopicId};
use super::value::StoredRtdValue;
use crate::generation::ConnectionGeneration;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use xlfn_kernel::operation_gate::{
    OperationGate, OperationGuard, OwnedOperationGuard, TerminationWaitGuard,
};
use xlfn_kernel::quota::Quota;

pub(crate) struct PublishCore<H: SubscriptionHost> {
    host: H,
    runtime_gate: NonNull<OperationGate>,
    server_gate: OperationGate,
    queued_update_quota: NonNull<Quota>,
    lifecycle: AtomicU8,
    publish_epoch: AtomicU64,
    notified_epoch: AtomicU64,
    pending_updates: AtomicUsize,
    deliverable_pending: AtomicUsize,
    // Incremental work index only; shard.deliverable_count and pending maps are authoritative.
    ready_shards: AtomicU32,
    shards: Box<[Mutex<TopicShard>]>,
    refresh: Mutex<RefreshState<H::Notifier>>,
    services: NonNull<RuntimeServices>,
}

impl<H: SubscriptionHost> PublishCore<H> {
    pub(crate) fn new(
        host: H,
        runtime_gate: &OperationGate,
        queued_update_quota: &Quota,
        services: &RuntimeServices,
    ) -> Self {
        let mut shards = Vec::with_capacity(super::delivery::TOPIC_SHARDS);
        for _ in 0..super::delivery::TOPIC_SHARDS {
            shards.push(Mutex::new(TopicShard::default()));
        }

        Self {
            host,
            runtime_gate: NonNull::from(runtime_gate),
            server_gate: OperationGate::new(),
            queued_update_quota: NonNull::from(queued_update_quota),
            lifecycle: AtomicU8::new(SERVER_LIFECYCLE_OPEN),
            publish_epoch: AtomicU64::new(0),
            notified_epoch: AtomicU64::new(u64::MAX),
            pending_updates: AtomicUsize::new(0),
            deliverable_pending: AtomicUsize::new(0),
            ready_shards: AtomicU32::new(0),
            shards: shards.into_boxed_slice(),
            refresh: Mutex::new(RefreshState::default()),
            services: NonNull::from(services),
        }
    }

    #[inline]
    fn runtime_gate(&self) -> &OperationGate {
        // SAFETY: SubscriptionRuntime uniquely owns the gate and reclaims it
        // only after every owned server and publish operation is drained.
        unsafe { self.runtime_gate.as_ref() }
    }

    #[inline]
    fn queued_update_quota(&self) -> &Quota {
        // SAFETY: the runtime-owned quota outlives all publish cores and their
        // queued-update permits.
        unsafe { self.queued_update_quota.as_ref() }
    }

    #[inline]
    fn services(&self) -> &RuntimeServices {
        // SAFETY: runtime services are reclaimed after all servers.
        unsafe { self.services.as_ref() }
    }
}

// SAFETY: every non-owning pointer targets an immutable-address field of the
// owning SubscriptionRuntime, which drains and drops all servers first.
unsafe impl<H: SubscriptionHost> Send for PublishCore<H> {}
// SAFETY: PublishCore fields use atomic or mutex synchronization.
unsafe impl<H: SubscriptionHost> Sync for PublishCore<H> {}

impl<H: SubscriptionHost> std::fmt::Debug for PublishCore<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishCore")
            .field("lifecycle", &self.lifecycle.load(Ordering::Relaxed))
            .field("publish_epoch", &self.publish_epoch.load(Ordering::Relaxed))
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
    pub(crate) _runtime_guard: OperationGuard<'a>,
    pub(crate) _server_guard: OperationGuard<'a>,
    pub(crate) _host_guard: H::AdmissionGuard,
    pub(crate) _observation: ServerOperationObservation,
}

pub(crate) struct OwnedPublishOperation<H: SubscriptionHost> {
    _runtime_guard: OwnedOperationGuard,
    _server_guard: OwnedOperationGuard,
    _host_guard: H::AdmissionGuard,
    _observation: ServerOperationObservation,
}

// SAFETY: the nested runtime and server operation guards admit execution
// across thread boundaries for the duration of the owned operation.
unsafe impl<H: SubscriptionHost> Send for OwnedPublishOperation<H> {}

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
    services: NonNull<RuntimeServices>,
}

// SAFETY: RuntimeServices is thread-safe, and ServerOperationObservation is
// only held within an admitted operation guard that guarantees runtime liveness.
unsafe impl Send for ServerOperationObservation {}

impl ServerOperationObservation {
    fn begin(services: &RuntimeServices) -> Self {
        services.record(crate::shutdown_trace::ShutdownEvent::BeginRtdOperation);
        Self {
            services: NonNull::from(services),
        }
    }
}

impl Drop for ServerOperationObservation {
    fn drop(&mut self) {
        // SAFETY: every observation is nested inside an admitted publish
        // operation, so runtime services cannot be reclaimed yet.
        unsafe { self.services.as_ref() }
            .record(crate::shutdown_trace::ShutdownEvent::EndRtdOperation);
    }
}

struct ServerCallbackObservation {
    services: NonNull<RuntimeServices>,
}

impl ServerCallbackObservation {
    fn begin(services: &RuntimeServices) -> Self {
        services.record(crate::shutdown_trace::ShutdownEvent::BeginCallback);
        Self {
            services: NonNull::from(services),
        }
    }
}

impl Drop for ServerCallbackObservation {
    fn drop(&mut self) {
        // SAFETY: callback observation is scoped by host/server admission.
        unsafe { self.services.as_ref() }.record(crate::shutdown_trace::ShutdownEvent::EndCallback);
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

        if let Some(active) = shard.active_by_topic.get_mut(&topic_id) {
            for buffer in [0, 1] {
                if let ValueSlot::Resident(versioned) = &active.values[buffer]
                    && versioned.generation == generation
                    && versioned.sequence <= delivered_sequence
                {
                    let is_latest = active.latest_slot == Some(buffer as u8);
                    let has_pending = shard.pending[buffer].contains_key(&topic_id);
                    if !is_latest && !has_pending {
                        active.values[buffer] = ValueSlot::Empty;
                    }
                }
            }
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
        let mut runtime_guard = None;
        let mut server_guard = None;
        let host_guard = self.host.enter_with(|| {
            runtime_guard = Some(self.runtime_gate().enter().map_err(|_| XllError::Closing)?);
            server_guard = Some(self.server_gate.enter().map_err(|_| XllError::Closing)?);
            Ok(())
        })?;

        Ok(ScopedPublishOperation {
            _runtime_guard: runtime_guard.expect("host admission acquires the runtime gate"),
            _server_guard: server_guard.expect("host admission acquires the server gate"),
            _host_guard: host_guard,
            _observation: ServerOperationObservation::begin(self.services()),
        })
    }

    pub(crate) fn enter_owned_operation(&self) -> XllResult<OwnedPublishOperation<H>> {
        let mut runtime_guard = None;
        let mut server_guard = None;
        let host_guard = self.host.enter_with(|| {
            // SAFETY: the runtime close waits this gate before reclaiming the
            // runtime and all server-owned publish cores.
            runtime_guard =
                Some(unsafe { self.runtime_gate().enter_owned() }.map_err(|_| XllError::Closing)?);
            // SAFETY: server termination drains this gate before reclaiming
            // the publish core.
            server_guard =
                Some(unsafe { self.server_gate.enter_owned() }.map_err(|_| XllError::Closing)?);
            Ok(())
        })?;
        let observation = ServerOperationObservation::begin(self.services());

        Ok(OwnedPublishOperation {
            _runtime_guard: runtime_guard.expect("host admission acquires the runtime gate"),
            _server_guard: server_guard.expect("host admission acquires the server gate"),
            _host_guard: host_guard,
            _observation: observation,
        })
    }

    pub(crate) fn reserve_connection(
        &self,
        topic_id: TopicId,
        id: SubscriptionId,
        generation: ConnectionGeneration,
        active_quota: &Quota,
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
            // SAFETY: the runtime-owned active quota outlives every server and
            // is reclaimed only after all connection permits are drained.
            let permit =
                unsafe { Quota::try_acquire(active_quota) }.map_err(|_| XllError::Overloaded)?;
            topic_entry.insert(topic_id);
            shard.active_by_topic.insert(
                topic_id,
                super::delivery::ActiveSubscription {
                    id,
                    generation,
                    committed: false,
                    values: [ValueSlot::Empty, ValueSlot::Empty],
                    latest_slot: None,
                    next_sequence: 0,
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

        let latest = active
            .latest_value()
            .cloned()
            .unwrap_or(StoredRtdValue::Empty);
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

    #[cfg_attr(feature = "hotpath", hotpath::measure(impl_type = "PublishCore"))]
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
                let active = active_by_topic
                    .get_mut(&topic_id)
                    .filter(|active| active.generation == generation)
                    .ok_or(XllError::Closing)?;
                if let Some(ValueSlot::Resident(latest)) = active.latest_slot_state()
                    && latest.value == value
                {
                    return Ok(());
                }
                let conn_gen = active.generation;
                let committed = active.committed;
                let sequence = active.allocate_sequence()?;
                let pending = &mut pending_buffers[buffer];
                let pending_entry = pending.entry(topic_id);
                let inserted = match pending_entry {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        let existing = entry.get_mut();
                        existing.connection_generation = conn_gen;
                        existing.sequence = sequence;
                        false
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        // SAFETY: the runtime-owned queue quota outlives this
                        // publish core and every queued-update permit.
                        let permit = unsafe { Quota::try_acquire(self.queued_update_quota()) }
                            .map_err(|_| XllError::Overloaded)?;
                        entry.insert(QueuedUpdate {
                            connection_generation: conn_gen,
                            sequence,
                            _permit: permit,
                        });
                        true
                    }
                };
                active.values[buffer] = ValueSlot::Resident(VersionedRtdValue {
                    generation: conn_gen,
                    sequence,
                    value,
                });
                active.latest_slot = Some(buffer as u8);
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
            let _callback = ServerCallbackObservation::begin(self.services());
            let res = catch_unwind(AssertUnwindSafe(|| self.host.notify(&attempt.notifier)));
            let completion = match res {
                Ok(Ok(())) => self.finish_notification_attempt(attempt.ticket, Ok(())),
                Ok(Err(err)) => self.finish_notification_attempt(attempt.ticket, Err(err)),
                Err(panic_payload) => {
                    let err = XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::PANIC_NOTIFY,
                    };
                    self.services().record_cleanup_result(Err(err.clone()));
                    std::panic::resume_unwind(panic_payload);
                }
            };

            match completion {
                NotificationCompletion::Finished => break,
                NotificationCompletion::Retry(next) => attempt = next,
                NotificationCompletion::Failed(err) => {
                    self.services().record_cleanup_result(Err(err));
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

    #[cfg_attr(feature = "hotpath", hotpath::measure(impl_type = "PublishCore"))]
    pub(crate) fn complete_refresh_inner(
        &self,
        refresh_id: u64,
        delivered_updates: Vec<RtdUpdate>,
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

        let mut updates_iter = delivered_updates.into_iter();
        let mut current_opt = updates_iter.next();
        while let Some(first_update) = current_opt {
            let shard_idx = shard_index(TopicId(first_update.topic_id));
            let mut shard = self.shards[shard_idx].lock();
            let mut current = first_update;
            loop {
                let topic_id = TopicId(current.topic_id);
                let slot = current.buffer as usize;

                match outcome {
                    RefreshOutcome::Delivered => {
                        if let Some(active) = shard.active_by_topic.get_mut(&topic_id)
                            && active.generation == current.connection_generation
                            && let ValueSlot::InFlight {
                                generation,
                                sequence,
                            } = active.values[slot]
                            && generation == current.connection_generation
                            && sequence == current.sequence
                        {
                            if active.latest_slot == Some(current.buffer) {
                                active.values[slot] = ValueSlot::Resident(VersionedRtdValue {
                                    generation,
                                    sequence,
                                    value: current.value,
                                });
                            } else {
                                active.values[slot] = ValueSlot::Empty;
                            }
                        }

                        self.retire_pending_through(
                            &mut shard,
                            shard_idx,
                            topic_id,
                            current.connection_generation,
                            current.sequence,
                        );
                    }
                    RefreshOutcome::Failed => {
                        if let Some(active) = shard.active_by_topic.get_mut(&topic_id)
                            && active.generation == current.connection_generation
                            && let ValueSlot::InFlight {
                                generation,
                                sequence,
                            } = active.values[slot]
                            && generation == current.connection_generation
                            && sequence == current.sequence
                        {
                            active.values[slot] = ValueSlot::Resident(VersionedRtdValue {
                                generation,
                                sequence,
                                value: current.value,
                            });
                        }
                    }
                }

                match updates_iter.next() {
                    Some(next_update)
                        if shard_index(TopicId(next_update.topic_id)) == shard_idx =>
                    {
                        current = next_update;
                    }
                    other => {
                        current_opt = other;
                        break;
                    }
                }
            }
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
}

fn push_deliverable(
    active_by_topic: &mut rustc_hash::FxHashMap<TopicId, super::delivery::ActiveSubscription>,
    updates: &mut Vec<RtdUpdate>,
    buffer: usize,
    topic_id: TopicId,
    queued: &QueuedUpdate,
) {
    let Some(active) = active_by_topic.get_mut(&topic_id) else {
        return;
    };
    if !active.committed || active.generation != queued.connection_generation {
        return;
    }
    let slot_entry = std::mem::replace(
        &mut active.values[buffer],
        ValueSlot::InFlight {
            generation: queued.connection_generation,
            sequence: queued.sequence,
        },
    );
    let ValueSlot::Resident(versioned) = slot_entry else {
        xlfn_kernel::invariant::fail_stop();
    };
    debug_assert_eq!(versioned.generation, queued.connection_generation);
    debug_assert_eq!(versioned.sequence, queued.sequence);
    updates.push(RtdUpdate {
        sequence: queued.sequence,
        topic_id: topic_id.0,
        connection_generation: queued.connection_generation,
        buffer: buffer as u8,
        value: versioned.value,
    });
}

impl<H: SubscriptionHost> PublishCore<H> {
    #[cfg_attr(feature = "hotpath", hotpath::measure(impl_type = "PublishCore"))]
    pub(crate) fn collect_shard(&self, shard_index: usize) -> Option<ShardRefreshBatch> {
        let mut shard = self.shards[shard_index].lock();
        if shard.deliverable_count == 0 {
            return None;
        }

        let TopicShard {
            active_by_topic,
            pending: [p0, p1],
            deliverable_count,
            ..
        } = &mut *shard;
        let mut updates = Vec::with_capacity(*deliverable_count);

        if p1.is_empty() {
            for (&topic_id, queued) in p0.iter() {
                push_deliverable(active_by_topic, &mut updates, 0, topic_id, queued);
            }
        } else if p0.is_empty() {
            for (&topic_id, queued) in p1.iter() {
                push_deliverable(active_by_topic, &mut updates, 1, topic_id, queued);
            }
        } else {
            for (&topic_id, queued_0) in p0.iter() {
                let queued_1 = p1.get(&topic_id);
                let Some(active) = active_by_topic.get(&topic_id) else {
                    continue;
                };
                if !active.committed {
                    continue;
                }
                let active_gen = active.generation;
                let d0 = queued_0.connection_generation == active_gen;
                let d1 = queued_1.is_some_and(|q1| q1.connection_generation == active_gen);

                let selected = match (d0, d1) {
                    (true, true) => {
                        let q1 = queued_1.expect("d1 implies queued_1 is Some");
                        if q1.sequence > queued_0.sequence {
                            (1, q1)
                        } else {
                            (0, queued_0)
                        }
                    }
                    (true, false) => (0, queued_0),
                    (false, true) => (1, queued_1.expect("d1 implies queued_1 is Some")),
                    (false, false) => continue,
                };

                let (buffer, queued) = selected;
                push_deliverable(active_by_topic, &mut updates, buffer, topic_id, queued);
            }

            for (&topic_id, queued_1) in p1.iter() {
                if !p0.contains_key(&topic_id) {
                    push_deliverable(active_by_topic, &mut updates, 1, topic_id, queued_1);
                }
            }
        }

        if updates.is_empty() {
            return None;
        }
        Some(ShardRefreshBatch {
            shard_index,
            updates,
        })
    }

    pub(crate) fn collect_refresh_batches(&self, plan: &RefreshPlan) -> Vec<ShardRefreshBatch> {
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
        batches
    }

    fn collect_refresh(&self, plan: &RefreshPlan) -> Vec<RtdUpdate> {
        reduce_refresh_batches(self.collect_refresh_batches(plan))
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

    #[cfg(feature = "bench-internals")]
    pub(crate) fn restore_refresh_batches(
        &self,
        plan: &RefreshPlan,
        batches: Vec<ShardRefreshBatch>,
    ) {
        let updates = reduce_refresh_batches(batches);
        self.restore_refresh_updates(plan, updates);
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn restore_refresh_updates(&self, plan: &RefreshPlan, updates: Vec<RtdUpdate>) {
        let _ = self.complete_refresh_inner(plan.refresh_id, updates, RefreshOutcome::Failed);
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
        self.finish_collection(updates)
    }

    pub(crate) fn finish_collection(mut self, updates: Vec<RtdUpdate>) -> RtdRefreshBatch<'a, H> {
        self.finished = true;
        RtdRefreshBatch {
            publish: self.publish,
            operation: self.operation.take(),
            plan: self.plan,
            updates,
            finished: false,
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
    publish: &'a PublishCore<H>,
    operation: Option<ScopedPublishOperation<'a, H>>,
    pub(crate) plan: RefreshPlan,
    pub(crate) updates: Vec<RtdUpdate>,
    finished: bool,
}

impl<H: SubscriptionHost> RtdRefreshBatch<'_, H> {
    pub(crate) fn complete(mut self, outcome: RefreshOutcome) -> XllResult<()> {
        self.finished = true;
        let updates = std::mem::take(&mut self.updates);
        let attempt =
            self.publish
                .complete_refresh_inner(self.plan.refresh_id, updates, outcome)?;
        if let Some(attempt) = attempt {
            self.publish.drive_notification(attempt);
        }
        self.operation.take();
        Ok(())
    }
}

// AUDIT [Lock Safety & Self-Deadlock Prevention]:
// `RtdRefreshBatch::drop` acquires shard mutexes during rollback (`complete_refresh_inner`).
// Self-deadlock is structurally impossible because:
// 1. `RtdRefreshBatch` is only constructed via `PlannedRtdRefresh::collect()` / `finish_collection()`,
//    which executes strictly after all `collect_shard` mutex guards have been dropped. No shard
//    lock is held when `RtdRefreshBatch` is handed to the caller.
// 2. Caller code (e.g. `IRtdServer::RefreshData`) and safe consumer APIs never acquire or hold
//    any shard mutexes.
// 3. Stack unwinding from panics in caller code or formatting therefore drops `RtdRefreshBatch`
//    with zero shard mutexes held by the current thread.
// 4. In `complete_refresh_inner`, shard mutexes are acquired sequentially and dropped
//    immediately without holding any other shard lock or the `refresh` lock.
impl<H: SubscriptionHost> Drop for RtdRefreshBatch<'_, H> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let updates = std::mem::take(&mut self.updates);
        let _ = self.publish.complete_refresh_inner(
            self.plan.refresh_id,
            updates,
            RefreshOutcome::Failed,
        );
    }
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub(crate) fn reduce_refresh_batches(batches: Vec<ShardRefreshBatch>) -> Vec<RtdUpdate> {
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
    updates
}
