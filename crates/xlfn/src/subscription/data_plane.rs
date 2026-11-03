#![allow(
    unsafe_code,
    reason = "publish permits borrow runtime-owned quotas proven live by RTD shutdown"
)]

mod refresh;
#[cfg(all(target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use refresh::RefreshOutputSlots;
pub(crate) use refresh::RtdRefreshBatch;
#[cfg(all(test, target_os = "windows", any(feature = "rtd", feature = "handles")))]
pub(crate) use refresh::write_refresh_data_for_test;
mod notification;
pub(crate) use notification::NotificationAttempt;

use super::delivery::{
    BufferIndex, DeliveryPhase, QueuedUpdate, RefreshOutcome, RefreshPlan, RefreshState, RtdUpdate,
    SERVER_LIFECYCLE_OPEN, SignalState, TopicShard, ValueSlot, VersionedRtdValue, shard_index,
};
use super::host::SubscriptionHost;
use super::runtime_services::RuntimeServices;
use super::topic::{SubscriptionId, TopicId};
use super::value::StoredRtdValue;
use crate::generation::ConnectionGeneration;
use crate::sync::Mutex;
use crate::{XllError, XllResult};
#[cfg(test)]
use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use triomphe::Arc;
use xlfn_kernel::operation_gate::{
    OperationGate, OperationGuard, OwnedOperationGuard, TerminationWaitGuard,
};
use xlfn_kernel::quota::Quota;

#[cfg(test)]
type OperationDropTrace = Arc<Mutex<Vec<&'static str>>>;

#[cfg(test)]
thread_local! {
    static OPERATION_DROP_TRACE: RefCell<Option<OperationDropTrace>> = const { RefCell::new(None) };
}

#[cfg(test)]
struct DropTrace {
    label: &'static str,
    trace: Option<OperationDropTrace>,
    recorded: bool,
}

#[cfg(test)]
impl DropTrace {
    fn new(label: &'static str) -> Self {
        Self {
            label,
            trace: OPERATION_DROP_TRACE.with(|trace| trace.borrow().clone()),
            recorded: false,
        }
    }

    fn record(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        if let Some(trace) = &self.trace {
            trace.lock().push(self.label);
        }
    }
}

#[cfg(test)]
impl Drop for DropTrace {
    fn drop(&mut self) {
        self.record();
    }
}

#[cfg(test)]
struct OperationDropTraceScope {
    previous: Option<OperationDropTrace>,
}

#[cfg(test)]
impl Drop for OperationDropTraceScope {
    fn drop(&mut self) {
        OPERATION_DROP_TRACE.with(|trace| {
            trace.replace(self.previous.take());
        });
    }
}

#[cfg(test)]
fn with_operation_drop_trace<R>(trace: OperationDropTrace, operation: impl FnOnce() -> R) -> R {
    let previous = OPERATION_DROP_TRACE.with(|current| current.replace(Some(trace)));
    let _scope = OperationDropTraceScope { previous };
    operation()
}

pub(crate) struct PublishCore<H: SubscriptionHost> {
    host: H,
    runtime_gate: NonNull<OperationGate>,
    server_gate: OperationGate,
    active_quota: NonNull<Quota>,
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
    /// Creates a publish core whose non-owning capabilities point into one
    /// subscription runtime.
    ///
    /// # Safety
    ///
    /// The owner must keep `services` at a stable address, without exclusive
    /// borrows, until this core and every capability it mints are dropped.
    pub(crate) unsafe fn new(host: H, services: &RuntimeServices) -> Self {
        let mut shards = Vec::with_capacity(super::delivery::TOPIC_SHARDS);
        for _ in 0..super::delivery::TOPIC_SHARDS {
            shards.push(Mutex::new(TopicShard::default()));
        }

        Self {
            host,
            runtime_gate: NonNull::from(&services.runtime_gate),
            server_gate: OperationGate::new(),
            active_quota: NonNull::from(&services.active_quota),
            queued_update_quota: NonNull::from(&services.queued_update_quota),
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
    fn active_quota(&self) -> &Quota {
        // SAFETY: the runtime-owned active quota outlives this publish core
        // and every active-subscription permit it creates.
        unsafe { self.active_quota.as_ref() }
    }

    #[inline]
    fn queued_update_quota(&self) -> &Quota {
        // SAFETY: the runtime-owned quota outlives all publish cores and their
        // queued-update permits.
        unsafe { self.queued_update_quota.as_ref() }
    }

    #[inline]
    pub(super) fn services(&self) -> &RuntimeServices {
        // SAFETY: runtime services are reclaimed after all servers.
        unsafe { self.services.as_ref() }
    }
}

// SAFETY: every non-owning pointer targets the stable RuntimeServices
// allocation, which the runtime reclaims after draining all servers.
unsafe impl<H: SubscriptionHost> Send for PublishCore<H> {}
// SAFETY: PublishCore fields use atomic or mutex synchronization.
unsafe impl<H: SubscriptionHost> Sync for PublishCore<H> {}

impl<H: SubscriptionHost> Drop for PublishCore<H> {
    fn drop(&mut self) {
        // An owned operation keeps a raw server-gate pointer and must never
        // survive reclamation of this core. Fail closed if the owner protocol
        // is violated instead of allowing its guard to become a UAF.
        if self.server_gate.active() != 0 {
            xlfn_kernel::invariant::fail_stop();
        }
    }
}

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
    // Drop first while both operation gates still keep runtime services live.
    pub(crate) _observation: ServerOperationObservation,
    pub(crate) _host_guard: H::AdmissionGuard,
    #[cfg(test)]
    _host_drop_trace: DropTrace,
    // Drop server admission before runtime admission.
    pub(crate) _server_guard: OperationGuard<'a>,
    #[cfg(test)]
    _server_drop_trace: DropTrace,
    pub(crate) _runtime_guard: OperationGuard<'a>,
    #[cfg(test)]
    _runtime_drop_trace: DropTrace,
}

pub(crate) struct OwnedPublishOperation<H: SubscriptionHost> {
    // Drop first while both operation gates still keep runtime services live.
    _observation: ServerOperationObservation,
    _host_guard: H::AdmissionGuard,
    #[cfg(test)]
    _host_drop_trace: DropTrace,
    // Drop server admission before runtime admission.
    _server_guard: OwnedOperationGuard,
    #[cfg(test)]
    _server_drop_trace: DropTrace,
    _runtime_guard: OwnedOperationGuard,
    #[cfg(test)]
    _runtime_drop_trace: DropTrace,
}

#[must_use = "wait for admitted publication before finishing termination"]
pub(crate) struct PublishTerminationStart<'a, H: SubscriptionHost> {
    publish: &'a PublishCore<H>,
    wait: TerminationWaitGuard<'a>,
    notifier: Option<H::Notifier>,
}

#[cfg(test)]
thread_local! {
    static PANIC_BEFORE_TERMINATION_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl<'a, H: SubscriptionHost> PublishTerminationStart<'a, H> {
    pub(crate) fn take_notifier(&mut self) -> Option<H::Notifier> {
        self.notifier.take()
    }

    pub(crate) fn wait(self) -> DrainedPublish<'a, H> {
        #[cfg(test)]
        if PANIC_BEFORE_TERMINATION_WAIT.replace(false) {
            panic!("injected publish termination wait panic");
        }
        self.wait.wait();
        DrainedPublish {
            publish: self.publish,
        }
    }
}

/// Completion authority for the exact publish core whose admission drained.
#[must_use = "finish termination after its publication admission has drained"]
pub(crate) struct DrainedPublish<'a, H: SubscriptionHost> {
    publish: &'a PublishCore<H>,
}

impl<H: SubscriptionHost> DrainedPublish<'_, H> {
    pub(crate) fn finish(self) -> PublishTerminationResult<H::Notifier> {
        self.publish.finish_termination()
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
    #[cfg(test)]
    drop_trace: DropTrace,
}

// SAFETY: RuntimeServices is thread-safe, and ServerOperationObservation is
// only held within an admitted operation guard that guarantees runtime liveness.
unsafe impl Send for ServerOperationObservation {}

impl ServerOperationObservation {
    fn begin(services: &RuntimeServices) -> Self {
        services.record(crate::shutdown_trace::ShutdownEvent::BeginRtdOperation);
        Self {
            services: NonNull::from(services),
            #[cfg(test)]
            drop_trace: DropTrace::new("observation"),
        }
    }
}

impl Drop for ServerOperationObservation {
    fn drop(&mut self) {
        #[cfg(test)]
        self.drop_trace.record();
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
        PublishTerminationStart {
            publish: self,
            wait,
            notifier,
        }
    }

    fn finish_termination(&self) -> PublishTerminationResult<H::Notifier> {
        let (notifier, recycled_updates) = {
            let mut refresh = self.refresh.lock();
            (
                refresh.detach_notifier(),
                std::mem::take(&mut refresh.recycled_updates),
            )
        };
        drop(recycled_updates);
        let mut connections = Vec::new();
        for shard_mutex in self.shards.iter() {
            // The permanent server tombstone still answers stale handles,
            // but no operation can reuse these tables after its gate drains.
            // Release their peak allocations instead of retaining clear maps.
            let shard = std::mem::take(&mut *shard_mutex.lock());
            connections.extend(shard.active_by_topic.into_values().map(|active| {
                RetiredConnection {
                    id: active.id,
                    generation: active.generation,
                }
            }));
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
            self.record_deliverable_increase(&mut shard.deliverable_count, shard_index, 1);
        }
    }

    #[inline]
    fn record_pending_removal(
        &self,
        deliverable_count: &mut usize,
        shard_index: usize,
        deliverable: bool,
    ) {
        let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.pending_updates);
        if deliverable {
            self.record_deliverable_decrease(deliverable_count, shard_index, 1);
        }
    }

    #[inline]
    fn record_deliverable_increase(
        &self,
        deliverable_count: &mut usize,
        shard_index: usize,
        count: usize,
    ) {
        if count == 0 {
            return;
        }
        let was_empty = *deliverable_count == 0;
        *deliverable_count = (*deliverable_count)
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
        deliverable_count: &mut usize,
        shard_index: usize,
        count: usize,
    ) {
        if count == 0 {
            return;
        }
        *deliverable_count = (*deliverable_count)
            .checked_sub(count)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        for _ in 0..count {
            let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.deliverable_pending);
        }
        if *deliverable_count == 0 {
            self.ready_shards
                .fetch_and(!(1_u32 << shard_index), Ordering::Release);
        }
    }

    fn remove_pending_if(
        &self,
        shard: &mut TopicShard,
        shard_index: usize,
        buffer: BufferIndex,
        topic_id: TopicId,
        predicate: impl FnOnce(&QueuedUpdate) -> bool,
    ) -> bool {
        if !remove_queued_if(&mut shard.pending[buffer.index()], topic_id, predicate) {
            return false;
        }
        let deliverable = shard
            .active_by_topic
            .get(&topic_id)
            .is_some_and(|active| active.committed);
        self.record_pending_removal(&mut shard.deliverable_count, shard_index, deliverable);
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
        for buffer in BufferIndex::ALL {
            self.remove_pending_if(shard, shard_index, buffer, topic_id, |queued| {
                queued.connection_generation == generation && queued.sequence <= delivered_sequence
            });
        }

        if let Some(active) = shard.active_by_topic.get_mut(&topic_id) {
            for buffer in BufferIndex::ALL {
                if let ValueSlot::Resident(versioned) = &active.values[buffer.index()]
                    && versioned.generation == generation
                    && versioned.sequence <= delivered_sequence
                {
                    let is_latest = active.latest_slot == Some(buffer);
                    let has_pending = shard.pending[buffer.index()].contains_key(&topic_id);
                    if !is_latest && !has_pending {
                        active.values[buffer.index()] = ValueSlot::Empty;
                    }
                }
            }
        }
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
            _observation: ServerOperationObservation::begin(self.services()),
            _host_guard: host_guard,
            #[cfg(test)]
            _host_drop_trace: DropTrace::new("host"),
            _server_guard: server_guard.expect("host admission acquires the server gate"),
            #[cfg(test)]
            _server_drop_trace: DropTrace::new("server"),
            _runtime_guard: runtime_guard.expect("host admission acquires the runtime gate"),
            #[cfg(test)]
            _runtime_drop_trace: DropTrace::new("runtime"),
        })
    }

    /// Enters an operation whose guard owns its gate admissions.
    ///
    /// # Safety
    ///
    /// The caller must keep this `PublishCore` and the runtime-owned fields
    /// referenced by it allocated until the returned operation is dropped.
    pub(crate) unsafe fn enter_owned_operation(&self) -> XllResult<OwnedPublishOperation<H>> {
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
            _observation: observation,
            _host_guard: host_guard,
            #[cfg(test)]
            _host_drop_trace: DropTrace::new("host"),
            _server_guard: server_guard.expect("host admission acquires the server gate"),
            #[cfg(test)]
            _server_drop_trace: DropTrace::new("server"),
            _runtime_guard: runtime_guard.expect("host admission acquires the runtime gate"),
            #[cfg(test)]
            _runtime_drop_trace: DropTrace::new("runtime"),
        })
    }

    pub(crate) fn reserve_connection(
        &self,
        topic_id: TopicId,
        id: SubscriptionId,
        generation: ConnectionGeneration,
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
            // SAFETY: `active_quota` is the quota coupled to this core at
            // construction, and the runtime reclaims it after its servers.
            let permit = unsafe { Quota::try_acquire_detached(self.active_quota()) }
                .map_err(|_| XllError::Overloaded)?;
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
    ) -> XllResult<Option<NotificationAttempt<'_, H>>> {
        let shard_index = shard_index(topic_id);
        let (epoch, has_pending) = {
            let mut shard = self.shards[shard_index].lock();
            self.ensure_open()?;
            let TopicShard {
                active_by_topic,
                pending,
                deliverable_count,
                ..
            } = &mut *shard;
            let Some(active) = active_by_topic.get_mut(&topic_id) else {
                return Err(XllError::Closing);
            };
            if active.generation != generation {
                return Err(XllError::Closing);
            }
            let was_committed = active.committed;
            if let Some(obs) = observed_sequence {
                for pending in pending.iter_mut() {
                    if remove_queued_if(pending, topic_id, |update| update.sequence <= obs) {
                        self.record_pending_removal(deliverable_count, shard_index, was_committed);
                    }
                }
            }
            let epoch = self.publish_epoch.load(Ordering::Acquire);
            let pending_count = pending
                .iter()
                .filter(|pending| {
                    pending.get(&topic_id).is_some_and(|update| {
                        update.connection_generation == generation
                            && observed_sequence.is_none_or(|seq| update.sequence > seq)
                    })
                })
                .count();
            // The exact mutable entry stays borrowed throughout pending updates;
            // no second lookup or reconstructed presence assertion is required.
            active.committed = true;
            if !was_committed && pending_count != 0 {
                self.record_deliverable_increase(deliverable_count, shard_index, pending_count);
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
    ) -> bool {
        let shard_index = shard_index(topic_id);
        let mut shard = self.shards[shard_index].lock();

        self.remove_pending_if(
            &mut shard,
            shard_index,
            BufferIndex::First,
            topic_id,
            |update| update.connection_generation == generation,
        );
        self.remove_pending_if(
            &mut shard,
            shard_index,
            BufferIndex::Second,
            topic_id,
            |update| update.connection_generation == generation,
        );

        let removed = shard
            .active_by_topic
            .get(&topic_id)
            .is_some_and(|active| active.generation == generation);
        if removed {
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
        removed
    }

    pub(crate) fn disconnect_connection(
        &self,
        topic_id: TopicId,
    ) -> XllResult<Option<RetiredConnection>> {
        let shard_index = shard_index(topic_id);
        let mut shard = self.shards[shard_index].lock();
        self.ensure_open()?;
        let TopicShard {
            active_by_topic,
            topic_by_id,
            pending,
            deliverable_count,
        } = &mut *shard;
        let std::collections::hash_map::Entry::Occupied(entry) = active_by_topic.entry(topic_id)
        else {
            return Ok(None);
        };
        let committed = entry.get().committed;
        for pending in pending.iter_mut() {
            if remove_queued_if(pending, topic_id, |_| true) {
                self.record_pending_removal(deliverable_count, shard_index, committed);
            }
        }
        // OccupiedEntry retains the selected connection through withdrawal.
        let (_, active) = entry.remove_entry();
        topic_by_id.remove(&active.id);
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
            let buffer = BufferIndex::from_epoch(epoch);

            let mut shard = self.shards[shard_index].lock();

            if self.publish_epoch.load(Ordering::Acquire) != epoch {
                drop(shard);
                continue;
            }

            let committed = self.commit_value(&mut shard, buffer, topic_id, generation, value)?;
            break (epoch, committed);
        };

        if committed && let Some(attempt) = self.prepare_notification_for_known_update(epoch)? {
            attempt.drive();
        }
        Ok(())
    }

    // Shared single-value commit keeps dedup, quotas, and sequence accounting
    // identical for direct sinks and bounded channel batches.
    // Only a changed value on a committed connection needs notification.
    #[inline]
    fn commit_value(
        &self,
        shard: &mut TopicShard,
        buffer: BufferIndex,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        value: StoredRtdValue,
    ) -> XllResult<bool> {
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
                return Ok(false);
            }
            let conn_gen = active.generation;
            let committed = active.committed;
            let sequence = active.allocate_sequence()?;
            let pending = &mut pending_buffers[buffer.index()];
            let pending_entry = pending.entry(topic_id);
            let inserted = match pending_entry {
                indexmap::map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    existing.connection_generation = conn_gen;
                    existing.sequence = sequence;
                    false
                }
                indexmap::map::Entry::Vacant(entry) => {
                    // SAFETY: the runtime-owned queue quota outlives this
                    // publish core and every queued-update permit.
                    let permit = unsafe { Quota::try_acquire_detached(self.queued_update_quota()) }
                        .map_err(|_| XllError::Overloaded)?;
                    entry.insert(QueuedUpdate {
                        connection_generation: conn_gen,
                        sequence,
                        _permit: permit,
                    });
                    true
                }
            };
            active.values[buffer.index()] = ValueSlot::Resident(VersionedRtdValue {
                generation: conn_gen,
                sequence,
                value,
            });
            active.latest_slot = Some(buffer);
            (committed, inserted)
        };
        if inserted {
            self.record_pending_insert(shard, shard_index(topic_id), committed);
        }
        Ok(committed)
    }

    /// Amortizes admission and shard locking over an already-notified FIFO
    /// batch. A notification boundary always unlocks and calls the host before
    /// the next value, preserving cancellation and reentrant refresh behavior.
    #[cfg(feature = "rtd")]
    pub(crate) fn publish_batch(
        &self,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        values: super::delivery::PublishBatch,
        stopping: &std::sync::atomic::AtomicBool,
    ) -> XllResult<()> {
        if values.is_empty() || stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        let _operation = self.enter_operation()?;
        let mut values = values.into_iter();
        while let Some(mut value) = values.next() {
            let (epoch, notify) = loop {
                self.ensure_open()?;
                let epoch = self.publish_epoch.load(Ordering::Acquire);
                let mut shard = self.shards[shard_index(topic_id)].lock();
                if self.publish_epoch.load(Ordering::Acquire) != epoch {
                    continue;
                }
                let notify = loop {
                    if stopping.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    self.ensure_open()?;
                    let committed = self.commit_value(
                        &mut shard,
                        BufferIndex::from_epoch(epoch),
                        topic_id,
                        generation,
                        value,
                    )?;
                    if committed && self.notified_epoch.load(Ordering::Acquire) != epoch {
                        break true;
                    }
                    // Refresh may rotate its epoch without this shard lock.
                    // Reacquire against the new epoch before the next value.
                    if self.publish_epoch.load(Ordering::Acquire) != epoch {
                        break false;
                    }
                    let Some(next) = values.next() else {
                        return Ok(());
                    };
                    value = next;
                };
                break (epoch, notify);
            };
            if notify && let Some(attempt) = self.prepare_notification_for_known_update(epoch)? {
                attempt.drive();
            }
        }
        Ok(())
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
            let attempt = self.prepare_notification(&mut refresh, has_updates)?;
            (retired, attempt)
        };
        if let Some(attempt) = attempt {
            attempt.drive();
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
            // A successful UpdateNotify only acknowledges the COM call, not
            // delivery: Excel may never pull that batch. Heartbeat retries
            // both failed and accepted notices while data remains pending.
            // Keep an in-flight call intact and reserve the replacement ticket
            // under this lock so concurrent heartbeats cannot overlap it.
            if has_updates
                && let DeliveryPhase::BetweenRefreshes {
                    signal: signal @ (SignalState::Suppressed { .. } | SignalState::Signaled { .. }),
                } = &mut refresh.phase
            {
                *signal = SignalState::Dormant;
            }
            self.prepare_notification(&mut refresh, has_updates)?
        };
        if let Some(attempt) = attempt {
            attempt.drive();
        }
        Ok(())
    }
}

fn remove_queued_if(
    pending: &mut indexmap::IndexMap<TopicId, QueuedUpdate, rustc_hash::FxBuildHasher>,
    topic_id: TopicId,
    predicate: impl FnOnce(&QueuedUpdate) -> bool,
) -> bool {
    let Some((&last_topic, last_update)) = pending.last() else {
        return false;
    };
    if last_topic == topic_id {
        if !predicate(last_update) {
            return false;
        }
        pending.pop();
    } else {
        let indexmap::map::Entry::Occupied(entry) = pending.entry(topic_id) else {
            return false;
        };
        if !predicate(entry.get()) {
            return false;
        }
        entry.swap_remove();
    }
    true
}

fn push_deliverable(
    active_by_topic: &mut rustc_hash::FxHashMap<TopicId, super::delivery::ActiveSubscription>,
    updates: &mut Vec<RtdUpdate>,
    buffer: BufferIndex,
    topic_id: TopicId,
    queued: &QueuedUpdate,
) {
    let Some(active) = active_by_topic.get_mut(&topic_id) else {
        return;
    };
    if !active.committed || active.generation != queued.connection_generation {
        return;
    }
    let slot = &mut active.values[buffer.index()];
    let ValueSlot::Resident(versioned) = slot else {
        // The queued index is an observation, not an ownership permission.
        // A stale/non-resident slot stays untouched and cannot issue an update.
        return;
    };
    if versioned.generation != queued.connection_generation || versioned.sequence != queued.sequence
    {
        return;
    }
    let value = std::mem::replace(&mut versioned.value, StoredRtdValue::Empty);
    *slot = ValueSlot::InFlight {
        generation: queued.connection_generation,
        sequence: queued.sequence,
    };
    updates.push(RtdUpdate {
        sequence: queued.sequence,
        topic_id: topic_id.0,
        connection_generation: queued.connection_generation,
        buffer,
        value,
    });
}

impl<H: SubscriptionHost> PublishCore<H> {
    fn collect_shard_into(&self, shard_index: usize, updates: &mut Vec<RtdUpdate>) {
        let mut shard = self.shards[shard_index].lock();
        if shard.deliverable_count == 0 {
            return;
        }

        let TopicShard {
            active_by_topic,
            pending: [p0, p1],
            ..
        } = &mut *shard;

        if p1.is_empty() {
            for (&topic_id, queued) in p0.iter() {
                push_deliverable(
                    active_by_topic,
                    updates,
                    BufferIndex::First,
                    topic_id,
                    queued,
                );
            }
        } else if p0.is_empty() {
            for (&topic_id, queued) in p1.iter() {
                push_deliverable(
                    active_by_topic,
                    updates,
                    BufferIndex::Second,
                    topic_id,
                    queued,
                );
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
                let deliverable_1 = queued_1.filter(|q1| q1.connection_generation == active_gen);

                let selected = match (d0, deliverable_1) {
                    (true, Some(q1)) => {
                        if q1.sequence > queued_0.sequence {
                            (BufferIndex::Second, q1)
                        } else {
                            (BufferIndex::First, queued_0)
                        }
                    }
                    (true, None) => (BufferIndex::First, queued_0),
                    (false, Some(q1)) => (BufferIndex::Second, q1),
                    (false, None) => continue,
                };

                let (buffer, queued) = selected;
                push_deliverable(active_by_topic, updates, buffer, topic_id, queued);
            }

            for (&topic_id, queued_1) in p1.iter() {
                if !p0.contains_key(&topic_id) {
                    push_deliverable(
                        active_by_topic,
                        updates,
                        BufferIndex::Second,
                        topic_id,
                        queued_1,
                    );
                }
            }
        }
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
    ) -> crate::sync::MutexGuard<'_, TopicShard> {
        self.shards[index].lock()
    }

    #[cfg(test)]
    pub(crate) fn mark_closing_for_test(&self) {
        self.lifecycle
            .store(super::delivery::SERVER_LIFECYCLE_CLOSING, Ordering::Release);
    }
}

#[cfg(all(test, feature = "rtd"))]
mod tests {
    use super::{OperationDropTrace, with_operation_drop_trace};
    use crate::subscription::runtime::SubscriptionRuntime;
    use triomphe::Arc;

    #[cfg(not(windows))]
    #[test]
    fn miri_refresh_selection_requires_the_exact_resident_payload() {
        use super::{TopicShard, push_deliverable};
        use crate::subscription::delivery::{BufferIndex, ValueSlot};

        let (_runtime, server, sink) =
            crate::subscription::tests::connected_sink::<i32>(None, "resident-authority");
        sink.publish(42).unwrap();
        let publish = &server.test_server().publish;
        let mut checked = false;
        for shard in &publish.shards {
            let mut shard = shard.lock();
            let TopicShard {
                active_by_topic,
                pending,
                ..
            } = &mut *shard;
            for (buffer, pending) in BufferIndex::ALL.into_iter().zip(pending.iter()) {
                let Some((&topic, queued)) = pending.first() else {
                    continue;
                };
                let original = active_by_topic[&topic].values[buffer.index()].clone();
                let ValueSlot::Resident(mut stale) = original.clone() else {
                    panic!("a fresh publish is resident");
                };
                stale.sequence += 1;
                let stale = ValueSlot::Resident(stale);
                active_by_topic.get_mut(&topic).unwrap().values[buffer.index()] = stale.clone();
                let mut updates = Vec::new();
                push_deliverable(active_by_topic, &mut updates, buffer, topic, queued);
                assert!(updates.is_empty());
                assert_eq!(active_by_topic[&topic].values[buffer.index()], stale);
                active_by_topic.get_mut(&topic).unwrap().values[buffer.index()] = ValueSlot::Empty;
                push_deliverable(active_by_topic, &mut updates, buffer, topic, queued);
                assert!(updates.is_empty());
                assert_eq!(
                    active_by_topic[&topic].values[buffer.index()],
                    ValueSlot::Empty
                );
                active_by_topic.get_mut(&topic).unwrap().values[buffer.index()] = original.clone();
                push_deliverable(active_by_topic, &mut updates, buffer, topic, queued);
                assert_eq!(updates.len(), 1);
                push_deliverable(active_by_topic, &mut updates, buffer, topic, queued);
                assert_eq!(updates.len(), 1);
                active_by_topic.get_mut(&topic).unwrap().values[buffer.index()] = original;
                checked = true;
            }
        }
        assert!(checked);
    }

    #[test]
    fn interrupted_termination_wait_preserves_undrained_publish_storage() {
        use crate::XllError;
        use crate::excel_rtd::RtdSubscriptionHost;
        use crate::subscription::delivery::SERVER_LIFECYCLE_CLOSING;
        use crate::subscription::server::TerminationAdmission;
        use crate::subscription::termination::TerminationPhase;
        use crate::subscription::tests::publishing_source;
        use crate::subscription::{RtdTopic, TopicId};
        use std::sync::atomic::Ordering;

        let (arena, source, sink_slot, disconnected) = publishing_source::<i32>(None);
        let runtime =
            SubscriptionRuntime::<RtdSubscriptionHost, _>::with_sources_for_internal(arena);
        let server = runtime.register_test_server(1);
        let prepared = runtime
            .prepare(
                &source,
                RtdTopic::single("interrupted-wait").unwrap().borrowed(),
            )
            .unwrap();
        server
            .connect_transaction(TopicId(1), prepared.id())
            .unwrap()
            .commit()
            .unwrap();
        prepared.commit();
        let publish = &server.test_server().publish;
        let connections = || {
            publish
                .shards
                .iter()
                .map(|shard| shard.lock().active_by_topic.len())
                .sum::<usize>()
        };
        let before = connections();
        assert!(before > 0);
        let admitted = publish.enter_operation().unwrap();
        let TerminationAdmission::Owner(owner) = server.test_server().begin_termination(&runtime)
        else {
            panic!("first caller must own termination");
        };
        let TerminationAdmission::Waiter(waiter) = server.test_server().begin_termination(&runtime)
        else {
            panic!("second caller must wait for termination");
        };
        assert!(!disconnected.load(Ordering::Acquire));
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let result = waiter.wait();
                assert!(disconnected.load(Ordering::Acquire));
                assert!(sink_slot.lock().is_none());
                result
            });
            super::PANIC_BEFORE_TERMINATION_WAIT.set(true);
            assert!(matches!(
                owner.request_cancel().finish(),
                Err(XllError::Panic)
            ));
            assert!(matches!(waiter.join().unwrap(), Err(XllError::Panic)));
        });

        assert_eq!(
            publish.lifecycle.load(Ordering::Acquire),
            SERVER_LIFECYCLE_CLOSING
        );
        assert_eq!(connections(), before);
        assert_eq!(
            server.test_server().termination_coordinator.phase(),
            TerminationPhase::Failed
        );
        assert!(matches!(server.terminate(), Err(XllError::Panic)));
        drop(admitted);
        assert!(matches!(runtime.close(), Err(XllError::Panic)));
    }

    #[test]
    fn refresh_rearm_preserves_nonblocking_publish_and_next_notification() {
        use crate::excel_rtd::RtdNotifier;
        use crate::rtd::test_support::TestNotifierState;
        use crate::subscription::tests::connected_sink;
        use crate::subscription::{RefreshOutcome, StoredRtdValue};
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        for completion in ["delivered", "failed", "dropped", "uncollected"] {
            let (_runtime, server, sink) = connected_sink::<i32>(None, completion);
            let notifier = std::sync::Arc::new(TestNotifierState::new());
            server
                .attach_update_notifier(RtdNotifier::for_test(std::sync::Arc::clone(&notifier)))
                .unwrap();
            sink.publish(1).unwrap();
            assert_eq!(notifier.calls.load(Ordering::Acquire), 1);
            let publish = &server.test_server().publish;
            let planned = publish.plan_refresh().unwrap();
            if completion == "uncollected" {
                sink.publish(2).unwrap();
                // No collected values: exercise the separate abort path.
                drop(planned);
            } else {
                let batch = planned.collect();
                sink.publish(2).unwrap();
                match completion {
                    "delivered" => batch.complete(RefreshOutcome::Delivered).unwrap(),
                    "failed" => batch.complete(RefreshOutcome::Failed).unwrap(),
                    _ => drop(batch),
                }
            }
            assert_eq!(notifier.calls.load(Ordering::Acquire), 2, "{completion}");

            // A pending notification must keep publishers independent of the
            // notification-control mutex, including after rearming a refresh.
            // Release the mutex before checking the timeout so a regression
            // fails rather than hanging while the scoped worker is joined.
            let result = std::thread::scope(|scope| {
                let refresh = publish.refresh.lock();
                let (sent, received) = std::sync::mpsc::channel();
                let sink = &sink;
                scope.spawn(move || sent.send(sink.publish(3)).unwrap());
                let result = received.recv_timeout(Duration::from_secs(5));
                drop(refresh);
                result
            });
            result
                .expect("a rearmed publisher must not wait for the notification mutex")
                .unwrap();
            assert_eq!(notifier.calls.load(Ordering::Acquire), 2, "{completion}");

            let latest = server.begin_refresh().unwrap();
            assert_eq!(latest.updates().len(), 1);
            assert_eq!(latest.updates()[0].value, StoredRtdValue::Integer(3));
            latest.complete(RefreshOutcome::Delivered).unwrap();
            sink.publish(4).unwrap();
            assert_eq!(notifier.calls.load(Ordering::Acquire), 3, "{completion}");
            let next = server.begin_refresh().unwrap();
            assert_eq!(next.updates()[0].value, StoredRtdValue::Integer(4));
            next.complete(RefreshOutcome::Delivered).unwrap();
        }
    }

    #[test]
    fn delayed_publish_notification_uses_the_current_refresh_epoch() {
        use crate::excel_rtd::RtdNotifier;
        use crate::rtd::test_support::TestNotifierState;
        use crate::subscription::RefreshOutcome;
        use std::sync::atomic::Ordering;

        let runtime = SubscriptionRuntime::<crate::excel_rtd::RtdSubscriptionHost>::new();
        let server = runtime.register_test_server(1);
        let publish = &server.test_server().publish;
        let notifier = std::sync::Arc::new(TestNotifierState::new());
        server
            .attach_update_notifier(RtdNotifier::for_test(std::sync::Arc::clone(&notifier)))
            .unwrap();
        let writer_epoch = publish.publish_epoch.load(Ordering::Acquire);
        server
            .begin_refresh()
            .unwrap()
            .complete(RefreshOutcome::Delivered)
            .unwrap();
        assert_eq!(notifier.calls.load(Ordering::Acquire), 0);

        // Model a writer delayed between releasing its shard and preparing a
        // notification. Its observed epoch predates a completed refresh; only
        // the notification-control lock determines the epoch reserved now.
        let attempt = publish
            .prepare_notification_for_known_update(writer_epoch)
            .unwrap()
            .unwrap();
        attempt.drive();
        let current_epoch = publish.publish_epoch.load(Ordering::Acquire);
        assert_ne!(writer_epoch, current_epoch);
        assert_eq!(
            publish.notified_epoch.load(Ordering::Acquire),
            current_epoch
        );
        assert_eq!(notifier.calls.load(Ordering::Acquire), 1);

        // Another stale writer cannot overwrite the already reserved epoch.
        assert!(
            publish
                .prepare_notification_for_known_update(writer_epoch)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            publish.notified_epoch.load(Ordering::Acquire),
            current_epoch
        );
    }

    #[test]
    fn terminated_servers_release_payload_storage_before_runtime_drop() {
        use crate::subscription::tests::SourceFixture;
        use crate::subscription::{RefreshOutcome, RtdTopic, TopicId};
        let fixture = SourceFixture::new();
        let sources = (0..64)
            .map(|_| fixture.add::<i32>(None))
            .collect::<Vec<_>>();
        let runtime =
            SubscriptionRuntime::<crate::excel_rtd::RtdSubscriptionHost, _>::with_sources_for_internal(
                fixture.finish(),
            );
        for generation in [1, 2] {
            let server = runtime.register_test_server(generation);
            for (index, (source, sink, _)) in sources.iter().enumerate() {
                let topic = RtdTopic::single(format!("footprint-{index}")).unwrap();
                let prepared = runtime.prepare(source, topic.borrowed()).unwrap();
                let id = prepared.id();
                prepared.commit();
                runtime
                    .connect_transaction(&server, TopicId(index as i32 + 1), id)
                    .unwrap()
                    .commit()
                    .unwrap();
                sink.lock().as_ref().unwrap().publish(index as i32).unwrap();
            }
            let batch = server.begin_refresh().unwrap();
            assert_eq!(batch.updates().len(), sources.len());
            batch.complete(RefreshOutcome::Delivered).unwrap();
            let publish = &server.test_server().publish;
            assert!(publish.refresh.lock().recycled_updates.capacity() >= sources.len());
            server.terminate().unwrap();
            assert!(matches!(
                server.begin_refresh(),
                Err(crate::XllError::Closing)
            ));
            assert_eq!(publish.refresh.lock().recycled_updates.capacity(), 0);
            assert_eq!(server.test_server().subscriptions.lock().capacity(), 0);
            for shard in &publish.shards {
                let shard = shard.lock();
                assert_eq!(shard.active_by_topic.capacity(), 0);
                assert_eq!(shard.topic_by_id.capacity(), 0);
                for pending in &shard.pending {
                    assert_eq!(pending.capacity(), 0);
                }
            }
        }
    }

    fn assert_drop_order(run: impl FnOnce()) {
        let trace: OperationDropTrace = Arc::new(crate::sync::Mutex::new(Vec::new()));
        with_operation_drop_trace(Arc::clone(&trace), run);
        assert_eq!(
            *trace.lock(),
            vec!["observation", "host", "server", "runtime"]
        );
    }

    #[test]
    fn scoped_publish_operation_drops_observation_before_liveness_guards() {
        let runtime = SubscriptionRuntime::<crate::excel_rtd::RtdSubscriptionHost>::new();
        let server = runtime.register_test_server(1);

        assert_drop_order(|| {
            let operation = server
                .test_server()
                .enter_operation()
                .expect("test server operation admission must succeed");
            drop(operation);
        });
    }

    #[test]
    fn owned_publish_operation_drops_observation_before_liveness_guards() {
        let runtime = SubscriptionRuntime::<crate::excel_rtd::RtdSubscriptionHost>::new();
        let server = runtime.register_test_server(1);

        assert_drop_order(|| {
            // SAFETY: `runtime` and `server` remain live until the owned
            // operation has been dropped in this scope.
            let operation = unsafe {
                server
                    .test_server()
                    .enter_owned_operation()
                    .expect("owned test server operation admission must succeed")
            };
            drop(operation);
        });
    }
}
