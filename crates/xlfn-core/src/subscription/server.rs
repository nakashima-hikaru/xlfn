use super::*;
use rustc_hash::FxHashMap;

#[derive(Clone)]
pub(crate) struct RtdServerHandle {
    pub(crate) inner: Arc<ServerRuntime>,
}

impl RtdServerHandle {
    pub(crate) fn attach_update_callback(
        &self,
        callback: NotificationCallback,
    ) -> XllResult<Option<NotificationCallback>> {
        let _operation = self.inner.enter_operation()?;
        let (retired, attempt) = {
            self.inner.ensure_open()?;
            let mut refresh = self.inner.refresh.lock();
            let retired = refresh.attach_callback(callback);
            let has_updates = self.inner.has_deliverable_updates();
            let prepared = refresh.prepare_notification(has_updates)?;
            let epoch = self.inner.publish_epoch.load(Ordering::Acquire);
            let attempt = prepared.map(|p| {
                self.inner.notified_epoch.store(epoch, Ordering::Release);
                refresh.commit_notification(p)
            });
            (retired, attempt)
        };
        if let Some(attempt) = attempt {
            self.inner.drive_notification(attempt);
        }
        Ok(retired)
    }

    pub(crate) fn detach_update_callback(&self) -> Option<NotificationCallback> {
        let mut refresh = self.inner.refresh.lock();
        refresh.detach_callback()
    }

    pub(crate) fn pulse_notification(&self) -> XllResult<()> {
        let _operation = self.inner.enter_operation()?;
        let attempt = {
            self.inner.ensure_open()?;
            let mut refresh = self.inner.refresh.lock();
            let has_updates = self.inner.has_deliverable_updates();
            let prepared = refresh.prepare_notification(has_updates)?;
            let epoch = self.inner.publish_epoch.load(Ordering::Acquire);
            prepared.map(|p| {
                self.inner.notified_epoch.store(epoch, Ordering::Release);
                refresh.commit_notification(p)
            })
        };
        if let Some(attempt) = attempt {
            self.inner.drive_notification(attempt);
        }
        Ok(())
    }

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch> {
        let operation = self.inner.enter_operation()?;
        let (refresh_id, updates) = {
            self.inner.ensure_open()?;
            let mut refresh = self.inner.refresh.lock();
            if matches!(refresh.phase, DeliveryPhase::Refreshing { .. }) {
                return Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::OVERLAPPED_REFERENCE,
                });
            }
            let refresh_id = refresh.next_refresh_id;
            refresh.next_refresh_id = refresh_id.checked_add(1).ok_or(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::REFERENCE_OVERFLOW,
            })?;

            self.inner.publish_epoch.fetch_add(1, Ordering::AcqRel);

            let mut by_topic: FxHashMap<i32, (u64, Arc<RtdValue>)> = FxHashMap::default();
            for shard_mutex in self.inner.shards.iter() {
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
                                    slot.insert((queued.sequence, Arc::clone(&queued.value)));
                                }
                                std::collections::hash_map::Entry::Occupied(mut slot) => {
                                    if queued.sequence > slot.get().0 {
                                        slot.insert((queued.sequence, Arc::clone(&queued.value)));
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
            server: Arc::clone(&self.inner),
            operation: Some(operation),
            refresh_id,
            updates,
            completed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        let epoch = self.inner.publish_epoch.load(Ordering::Acquire);
        let buf0 = (epoch & 1) as usize;
        let buf1 = 1 - buf0;
        let mut count = 0;
        for shard_mutex in self.inner.shards.iter() {
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
    ) -> XllResult<SubscriptionConnection> {
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

pub(crate) struct ServerRuntime {
    pub(crate) generation: ServerGeneration,
    pub(crate) module_ingress: Option<&'static crate::ingress::ExportIngress>,
    pub(crate) operation_gate: Arc<OperationGate>,
    pub(crate) lifecycle: AtomicU8,
    pub(crate) publish_epoch: AtomicU64,
    pub(crate) next_update_sequence: AtomicU64,
    pub(crate) notified_epoch: AtomicU64,
    pub(crate) pending_updates: AtomicUsize,
    pub(crate) shards: Box<[Mutex<TopicShard>]>,
    pub(crate) refresh: Mutex<RefreshState>,
    pub(crate) parent: Weak<SubscriptionRuntime>,
    pub(crate) termination_coordinator: TerminationCoordinator,
}

pub(crate) struct ServerOperation {
    pub(crate) _gate_guard: OperationGuard,
    pub(crate) _ingress_guard: Option<crate::ingress::ExportCallGuard<'static>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) parent: Weak<SubscriptionRuntime>,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl Drop for ServerOperation {
    fn drop(&mut self) {
        if let Some(parent) = self.parent.upgrade() {
            parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
    }
}

impl ServerRuntime {
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

    pub(crate) fn ensure_notified(self: &Arc<Self>, epoch: u64) -> XllResult<()> {
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

    pub(crate) fn enter_operation(&self) -> XllResult<ServerOperation> {
        let parent = self.parent.upgrade().ok_or(XllError::Closing)?;
        if (parent.runtime_gate.state.load(Ordering::Acquire) & CLOSING_BIT) != 0 {
            return Err(XllError::Closing);
        }

        if let Some(ingress) = self.module_ingress {
            let mut gate_guard = None;
            let mut gate_error = None;
            let (ingress_guard, accepted) =
                ingress.enter_with(|| match self.operation_gate.enter() {
                    Ok(guard) => {
                        gate_guard = Some(guard);
                        #[cfg(any(test, feature = "shutdown-refinement"))]
                        if let Some(parent) = self.parent.upgrade() {
                            parent.record_ghost_event(
                                crate::shutdown_refinement::GhostEvent::BeginRtdOperation,
                            );
                        }
                    }
                    Err(err) => gate_error = Some(err),
                });
            if !accepted {
                return Err(XllError::Closing);
            }
            if let Some(err) = gate_error {
                drop(ingress_guard);
                return Err(err);
            }
            Ok(ServerOperation {
                _gate_guard: gate_guard.expect("gate guard is acquired"),
                _ingress_guard: Some(ingress_guard),
                #[cfg(any(test, feature = "shutdown-refinement"))]
                parent: self.parent.clone(),
            })
        } else {
            let gate_guard = self.operation_gate.enter()?;
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
            }
            Ok(ServerOperation {
                _gate_guard: gate_guard,
                _ingress_guard: None,
                #[cfg(any(test, feature = "shutdown-refinement"))]
                parent: self.parent.clone(),
            })
        }
    }

    pub(crate) fn publish(
        self: &Arc<Self>,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        value: RtdValue,
    ) -> XllResult<()> {
        let _operation = self.enter_operation()?;
        value.validate()?;

        let shard_index = shard_index(topic_id);

        let value = Arc::new(value);

        let epoch = loop {
            self.ensure_open()?;

            let epoch = self.publish_epoch.load(Ordering::Acquire);
            let buffer = (epoch & 1) as usize;

            let mut shard = self.shards[shard_index].lock();

            if self.publish_epoch.load(Ordering::Acquire) != epoch {
                drop(shard);
                continue;
            }

            let conn_gen = {
                let active = shard
                    .active_by_topic
                    .get(&topic_id)
                    .filter(|active| active.generation == generation)
                    .ok_or(XllError::Closing)?;
                active.generation
            };

            let is_new_update = !shard.pending[buffer].contains_key(&topic_id);

            let parent = self.parent.upgrade().ok_or(XllError::Closing)?;
            let permit = if is_new_update {
                Some(parent.queued_update_quota.try_acquire()?)
            } else {
                None
            };

            let sequence = self.next_update_sequence.fetch_add(1, Ordering::Relaxed);

            match shard.pending[buffer].entry(topic_id) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    existing.connection_generation = conn_gen;
                    existing.sequence = sequence;
                    existing.value = Arc::clone(&value);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(QueuedUpdate {
                        connection_generation: conn_gen,
                        sequence,
                        value: Arc::clone(&value),
                        _permit: permit.expect("new update owns quota permit"),
                    });
                    self.pending_updates.fetch_add(1, Ordering::Relaxed);
                }
            }

            if let Some(active) = shard.active_by_topic.get_mut(&topic_id) {
                active.latest = Arc::clone(&value);
            }
            break epoch;
        };

        self.ensure_notified(epoch)?;
        Ok(())
    }

    pub(crate) fn drive_notification(self: &Arc<Self>, mut attempt: NotificationAttempt) {
        loop {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginCallback);
            }
            let res = catch_unwind(AssertUnwindSafe(|| (attempt.callback)()));
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let Some(parent) = self.parent.upgrade() {
                parent.record_ghost_event(crate::shutdown_refinement::GhostEvent::EndCallback);
            }
            let completion = match res {
                Ok(Ok(())) => self.finish_notification_attempt(attempt.ticket, Ok(())),
                Ok(Err(err)) => self.finish_notification_attempt(attempt.ticket, Err(err)),
                Err(panic_payload) => {
                    let err = XllError::Internal {
                        diagnostic_id: crate::DiagnosticId::PANIC_NOTIFY,
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
    ) -> NotificationCompletion {
        let mut refresh = self.refresh.lock();
        let callback = refresh.callback.clone();
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
                    if let Some(callback) = callback {
                        NotificationCompletion::Retry(NotificationAttempt { ticket, callback })
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
    ) -> XllResult<Option<NotificationAttempt>> {
        let mut refresh = self.refresh.lock();
        let DeliveryPhase::Refreshing {
            refresh_id: active_id,
        } = refresh.phase
        else {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::NO_REFERENCE,
            });
        };

        if active_id != refresh_id {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::REFERENCE_ID_MISMATCH,
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

    pub(crate) fn abort_refresh_no_unwind(self: &Arc<Self>, refresh_id: u64) {
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

    pub(crate) fn remove_from_registry(&self) {
        if let Some(parent) = self.parent.upgrade() {
            let mut servers = parent.servers.lock();
            servers.remove(&self.generation);
        }
    }

    pub(crate) fn begin_termination<'a>(self: &'a Arc<Self>) -> TerminationAdmission<'a> {
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
                let wait = self.operation_gate.close_and_wait_begin();
                term_state.phase = ServerTerminationPhase::Terminating;

                self.lifecycle
                    .store(SERVER_LIFECYCLE_CLOSING, Ordering::Release);

                let callback = {
                    let mut refresh = self.refresh.lock();
                    refresh.detach_callback()
                };

                let mut initial_subscriptions = Vec::new();
                for shard_mutex in self.shards.iter() {
                    let mut shard = shard_mutex.lock();
                    shard.pending[0].clear();
                    shard.pending[1].clear();
                    for active in shard.active_by_topic.values_mut() {
                        if let Some(sub) = active.subscription.take() {
                            initial_subscriptions.push(sub);
                        }
                    }
                }
                self.pending_updates.store(0, Ordering::Release);

                TerminationAdmission::Owner(ServerTermination {
                    server: Arc::clone(self),
                    wait,
                    callback,
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

pub(crate) enum TerminationAdmission<'a> {
    Owner(ServerTermination<'a>),
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

pub(crate) fn drop_callback_no_unwind(callback: Option<NotificationCallback>) -> XllResult<()> {
    catch_unwind(AssertUnwindSafe(|| drop(callback))).map_err(|_| XllError::Panic)
}

pub(crate) struct TerminatedTopic {
    pub(crate) key: SubscriptionKey,
    pub(crate) generation: ConnectionGeneration,
    pub(crate) subscription: Option<Box<dyn RtdSubscription>>,
}

thread_local! {
    pub(crate) static PANIC_AFTER_TERMINATION_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) struct ServerTermination<'a> {
    pub(crate) server: Arc<ServerRuntime>,
    pub(crate) wait: TerminationWaitGuard<'a>,
    pub(crate) callback: Option<NotificationCallback>,
    pub(crate) initial_subscriptions: Vec<Box<dyn RtdSubscription>>,
}

impl<'a> ServerTermination<'a> {
    pub(crate) fn request_cancel(&self) -> XllResult<()> {
        let mut first_error = None;
        for sub in &self.initial_subscriptions {
            if catch_unwind(AssertUnwindSafe(|| sub.request_cancel())).is_err()
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
        if PANIC_AFTER_TERMINATION_GUARD.with(|cell| cell.replace(false)) {
            panic!("injected termination owner panic");
        }

        let mut first_error = cancel_result.err();

        if let Err(err) = drop_callback_no_unwind(self.callback.take())
            && first_error.is_none()
        {
            first_error = Some(err);
        }

        let wait_res = catch_unwind(AssertUnwindSafe(|| self.wait.wait()));
        if wait_res.is_err() && first_error.is_none() {
            first_error = Some(XllError::Panic);
        }

        let (late_callback, active_entries) = {
            let late_callback = self.server.refresh.lock().detach_callback();
            let mut active_entries = Vec::new();
            for shard_mutex in self.server.shards.iter() {
                let mut shard = shard_mutex.lock();
                shard.pending[0].clear();
                shard.pending[1].clear();
                for (_, active) in shard.active_by_topic.drain() {
                    active_entries.push(TerminatedTopic {
                        key: active.key,
                        generation: active.generation,
                        subscription: active.subscription,
                    });
                }
                shard.topic_by_key.clear();
            }
            self.server.pending_updates.store(0, Ordering::Release);
            self.server
                .lifecycle
                .store(SERVER_LIFECYCLE_TERMINATED, Ordering::Release);

            (late_callback, active_entries)
        };

        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(parent) = self.server.parent.upgrade() {
            for _ in 0..self.initial_subscriptions.len() {
                parent
                    .record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveSubscription);
            }
        }

        if let Err(err) = drop_callback_no_unwind(late_callback)
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
                .pending
                .iter()
                .filter(|(_, p)| p.server_generation == Some(self.server.generation))
                .map(|(k, _)| k.clone())
                .collect();

            let mut extra_sources = Vec::new();
            for key in unactive_pending_keys {
                let should_remove = catalog.pending.get_mut(&key).is_some_and(|pending| {
                    pending.server_generation = None;
                    pending.connecting_generation = None;
                    pending.committed = false;
                    pending.live_reservations == 0
                });

                if should_remove {
                    let Some(removed) = catalog.pending.remove(&key) else {
                        continue;
                    };
                    catalog.pending_topic_bytes = catalog
                        .pending_topic_bytes
                        .saturating_sub(removed.topic.byte_len());
                    extra_sources.push(removed.source);
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

        let all_subscriptions = self
            .initial_subscriptions
            .drain(..)
            .chain(active_entries.into_iter().filter_map(|e| e.subscription));

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
pub(crate) struct RtdRefreshBatch {
    pub(crate) server: Arc<ServerRuntime>,
    pub(crate) operation: Option<ServerOperation>,
    pub(crate) refresh_id: u64,
    pub(crate) updates: Vec<RtdUpdate>,
    pub(crate) completed: bool,
}

impl RtdRefreshBatch {
    pub(crate) fn complete(mut self, outcome: RefreshOutcome) -> XllResult<()> {
        let attempt =
            self.server
                .complete_refresh_inner(self.refresh_id, &self.updates, outcome)?;
        if let Some(attempt) = attempt {
            self.server.drive_notification(attempt);
        }
        self.completed = true;
        self.operation.take();
        Ok(())
    }
}

impl Drop for RtdRefreshBatch {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.server.abort_refresh_no_unwind(self.refresh_id);
    }
}

pub(crate) fn disconnect_one_no_unwind(subscription: Box<dyn RtdSubscription>) -> XllResult<()> {
    match catch_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait())) {
        Ok(result) => result,
        Err(_) => Err(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::PANIC_DISCONNECT,
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
    if catalog
        .active_keys
        .get(key)
        .is_some_and(|binding| binding.connection_generation == conn_generation)
    {
        catalog.active_keys.remove(key);
    }

    if let Some(pending) = catalog
        .pending
        .get_mut(key)
        .filter(|p| p.server_generation == Some(server_generation))
    {
        if pending.connecting_generation == Some(conn_generation) {
            pending.connecting_generation = None;
        }
        pending.server_generation = None;
        pending.committed = false;
    }

    let res = if catalog.pending.get(key).is_some_and(|p| {
        p.connecting_generation.is_none()
            && p.server_generation.is_none()
            && p.live_reservations == 0
    }) {
        let removed = catalog.pending.remove(key);
        if let Some(removed) = removed {
            let bytes = removed.topic.byte_len();
            catalog.pending_topic_bytes = catalog.pending_topic_bytes.saturating_sub(bytes);
            Some(removed.source)
        } else {
            None
        }
    } else {
        None
    };

    remove_identity_if_unbound(catalog, key);
    res
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
                diagnostic_id: crate::DiagnosticId::TOPIC_ID_DUPLICATE,
            },
            ServerReservationFailure::DuplicateKey => XllError::Internal {
                diagnostic_id: crate::DiagnosticId::TOPIC_KEY_DUPLICATE,
            },
            ServerReservationFailure::Overloaded(err) => err,
        }
    }
}
