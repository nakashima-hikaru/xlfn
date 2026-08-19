use super::*;
use rustc_hash::FxHashMap;

#[cfg(test)]
pub(crate) type OperationEnterHook = Arc<dyn Fn() + Send + Sync + 'static>;

pub(crate) struct SubscriptionRuntime {
    pub(crate) runtime_id: u64,
    pub(crate) limits: RtdLimits,
    pub(crate) module_ingress: Option<&'static crate::ingress::ExportIngress>,
    pub(crate) runtime_gate: OperationGate,
    pub(crate) catalog: Mutex<SubscriptionCatalog>,
    pub(crate) servers: Mutex<FxHashMap<ServerGeneration, Arc<ServerRuntime>>>,
    pub(crate) active_quota: Arc<Quota>,
    pub(crate) queued_update_quota: Arc<Quota>,
    pub(crate) cleanup_failure: Mutex<Option<XllError>>,
    pub(crate) next_preparation_id: AtomicU64,
    pub(crate) next_connection_generation: AtomicU64,
    pub(crate) termination_coordinator: TerminationCoordinator,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    pub(crate) test_enter_hook: Mutex<Option<OperationEnterHook>>,
}

impl SubscriptionRuntime {
    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn new() -> Self {
        Self::with_limits(RtdLimits::standard())
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn with_limits(limits: RtdLimits) -> Self {
        Self::with_limits_and_ingress(limits, None)
    }

    pub(crate) fn with_module_ingress(limits: RtdLimits) -> Self {
        Self::with_limits_and_ingress(limits, Some(crate::ingress::global_ingress()))
    }

    fn with_limits_and_ingress(
        limits: RtdLimits,
        module_ingress: Option<&'static crate::ingress::ExportIngress>,
    ) -> Self {
        let runtime_id = allocate_runtime_id().expect("runtime ID allocation overflow");
        Self {
            runtime_id,
            limits,
            module_ingress,
            runtime_gate: OperationGate::new(),
            catalog: Mutex::new(SubscriptionCatalog {
                pending: HashMap::new(),
                pending_topic_bytes: 0,
                sources: SourceIdentityRegistry::new(),
                active_keys: HashMap::new(),
                identities: SubscriptionIdentityIndex::default(),
                next_subscription_id: 1,
            }),
            servers: Mutex::new(FxHashMap::default()),
            active_quota: Arc::new(Quota::new(limits.max_active)),
            queued_update_quota: Arc::new(Quota::new(limits.max_queued_updates)),
            cleanup_failure: Mutex::new(None),
            next_preparation_id: AtomicU64::new(1),
            next_connection_generation: AtomicU64::new(1),
            termination_coordinator: TerminationCoordinator::default(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(test)]
            test_enter_hook: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_operation_enter_hook(&self, hook: Option<OperationEnterHook>) {
        *self.test_enter_hook.lock() = hook;
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(event);
        }
    }

    pub(crate) fn record_cleanup_result(&self, result: XllResult<()>) {
        if let Err(error) = result {
            let mut failure = self.cleanup_failure.lock();
            if failure.is_none() {
                *failure = Some(error);
            }
        }
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup_failure
            .lock()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    pub(crate) fn enter_external_operation(&self) -> XllResult<OperationGuard<'_>> {
        self.runtime_gate.enter()
    }

    pub(crate) fn register_server(
        self: &Arc<Self>,
        generation: ServerGeneration,
    ) -> XllResult<RtdServerHandle> {
        let _operation = self.runtime_gate.enter()?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        let mut shards = Vec::with_capacity(TOPIC_SHARDS);
        for _ in 0..TOPIC_SHARDS {
            shards.push(Mutex::new(TopicShard::default()));
        }
        let server = Arc::new(ServerRuntime {
            generation,
            module_ingress: self.module_ingress,
            operation_gate: OperationGate::new(),
            lifecycle: AtomicU8::new(SERVER_LIFECYCLE_OPEN),
            publish_epoch: AtomicU64::new(0),
            next_update_sequence: AtomicU64::new(0),
            notified_epoch: AtomicU64::new(u64::MAX),
            pending_updates: AtomicUsize::new(0),
            shards: shards.into_boxed_slice(),
            refresh: Mutex::new(RefreshState::default()),
            parent: Arc::downgrade(self),
            termination_coordinator: TerminationCoordinator::default(),
        });

        let mut servers = self.servers.lock();
        if servers.contains_key(&generation) {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::RTD_SERVER_DUE,
            });
        }
        servers.insert(generation, Arc::clone(&server));
        Ok(RtdServerHandle { inner: server })
    }

    pub(crate) fn prepare<S>(
        self: &Arc<Self>,
        source: Arc<S>,
        topic: RtdTopic,
    ) -> XllResult<PreparedSubscription>
    where
        S: RtdSource,
    {
        let _operation = self.runtime_gate.enter()?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        topic.validate_with_limits(&self.limits)?;

        let source: Arc<dyn ErasedRtdSource> = source;
        let mut catalog = self.catalog.lock();

        let source_identity = catalog
            .sources
            .resolve(&source, self.limits.max_source_ids)?;
        let source_id = source_identity.id;

        let identity = SubscriptionIdentity {
            source_id: SourceId(source_id),
            topic: topic.clone(),
        };

        if let Some(existing_key) = catalog.identities.get_key(&identity).cloned() {
            if catalog.active_keys.contains_key(&existing_key) {
                return Ok(PreparedSubscription {
                    runtime: Arc::downgrade(self),
                    key: existing_key,
                    reservation_id: None,
                    ownership: PreparationOwnership::ExistingActive,
                });
            }

            if let Some(pending) = catalog.pending.get_mut(&existing_key) {
                let reservation_id = self.next_preparation_id.fetch_add(1, Ordering::Relaxed);

                pending.live_reservations =
                    pending
                        .live_reservations
                        .checked_add(1)
                        .ok_or(XllError::Internal {
                            diagnostic_id: crate::DiagnosticId::RESERVATION_OVERFLOW,
                        })?;

                return Ok(PreparedSubscription {
                    runtime: Arc::downgrade(self),
                    key: existing_key,
                    reservation_id: Some(reservation_id),
                    ownership: PreparationOwnership::ExistingPending,
                });
            }

            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::RTD_INDEX_ORPHAN,
            });
        }

        if catalog.pending.len() >= self.limits.max_pending {
            catalog.sources.rollback_registration(source_identity);
            return Err(XllError::Overloaded);
        }

        let new_total = match catalog.pending_topic_bytes.checked_add(topic.byte_len()) {
            Some(total) if total <= self.limits.max_total_topic_bytes => total,
            _ => {
                catalog.sources.rollback_registration(source_identity);
                return Err(XllError::Overloaded);
            }
        };

        let key = catalog.allocate_transport_key(self.runtime_id)?;

        catalog.identities.insert(identity, key.clone())?;

        let reservation_id = self.next_preparation_id.fetch_add(1, Ordering::Relaxed);
        catalog.pending_topic_bytes = new_total;
        catalog.pending.insert(
            key.clone(),
            PendingSubscription {
                live_reservations: 1,
                committed: false,
                source,
                topic,
                server_generation: None,
                connecting_generation: None,
            },
        );

        Ok(PreparedSubscription {
            runtime: Arc::downgrade(self),
            key,
            reservation_id: Some(reservation_id),
            ownership: PreparationOwnership::CreatedPending,
        })
    }

    pub(crate) fn finish_preparation(
        &self,
        key: &SubscriptionKey,
        _reservation_id: u64,
        committed: bool,
    ) {
        let (removed_source, _removed_topic_bytes) = {
            let mut catalog = self.catalog.lock();
            let Some(pending) = catalog.pending.get_mut(key) else {
                return;
            };

            pending.live_reservations = pending.live_reservations.saturating_sub(1);
            if committed {
                pending.committed = true;
                return;
            }

            if pending.live_reservations == 0
                && !pending.committed
                && pending.connecting_generation.is_none()
            {
                let removed = catalog.pending.remove(key);
                if let Some(removed) = removed {
                    let bytes = removed.topic.byte_len();
                    catalog.pending_topic_bytes = catalog.pending_topic_bytes.saturating_sub(bytes);
                    remove_identity_if_unbound(&mut catalog, key);
                    (Some(removed.source), bytes)
                } else {
                    (None, 0)
                }
            } else {
                (None, 0)
            }
        };

        if let Some(source) = removed_source {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::PANIC_SOURCE,
                }));
            }
        }
    }

    pub(crate) fn claim_server_key(
        &self,
        generation: ServerGeneration,
        key: &SubscriptionKey,
    ) -> XllResult<()> {
        let mut catalog = self.catalog.lock();
        let pending = catalog.pending.get_mut(key).ok_or(XllError::Closing)?;

        if let Some(existing_gen) = pending.server_generation {
            if existing_gen != generation {
                return Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::SERVER_GENERATION_MISMATCH,
                });
            }
        } else {
            pending.server_generation = Some(generation);
        }
        Ok(())
    }

    pub(crate) fn rollback_catalog_connection_reservation(
        &self,
        key: &SubscriptionKey,
        generation: ConnectionGeneration,
    ) {
        let mut catalog = self.catalog.lock();

        if let Some(pending) = catalog
            .pending
            .get_mut(key)
            .filter(|p| p.connecting_generation == Some(generation))
        {
            pending.connecting_generation = None;
        }

        if catalog
            .active_keys
            .get(key)
            .is_some_and(|binding| binding.connection_generation == generation)
        {
            catalog.active_keys.remove(key);
        }

        remove_identity_if_unbound(&mut catalog, key);
    }

    pub(crate) fn connect_transaction(
        self: &Arc<Self>,
        server_handle: &RtdServerHandle,
        topic_id: TopicId,
        key: &SubscriptionKey,
    ) -> XllResult<SubscriptionConnection> {
        let operation = server_handle.inner.enter_owned_operation()?;
        let conn_gen = ConnectionGeneration(
            self.next_connection_generation
                .fetch_add(1, Ordering::Relaxed),
        );

        let (source, topic) = {
            let mut catalog = self.catalog.lock();

            if catalog.active_keys.contains_key(key) {
                return Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::ACTIVE_KEY_DUPLICATE,
                });
            }

            let (source, topic) = {
                let pending = catalog.pending.get_mut(key).ok_or(XllError::Closing)?;

                if let Some(existing_gen) = pending.server_generation {
                    if existing_gen != server_handle.inner.generation {
                        return Err(XllError::Internal {
                            diagnostic_id: crate::DiagnosticId::SERVER_GENERATION_MISMATCH,
                        });
                    }
                } else {
                    pending.server_generation = Some(server_handle.inner.generation);
                }

                if pending.connecting_generation.is_some() {
                    return Err(XllError::Internal {
                        diagnostic_id: crate::DiagnosticId::CONNECTION_INFLIGHT,
                    });
                }
                pending.connecting_generation = Some(conn_gen);
                (Arc::clone(&pending.source), pending.topic.clone())
            };

            catalog.active_keys.insert(
                key.clone(),
                ActiveKeyBinding {
                    connection_generation: conn_gen,
                    stage: BindingStage::Connecting,
                },
            );

            (source, topic)
        };

        let shard_index = shard_index(topic_id);
        let reservation_result = {
            let mut shard = server_handle.inner.shards[shard_index].lock();

            if let Err(err) = server_handle.inner.ensure_open() {
                Err(ServerReservationFailure::Overloaded(err))
            } else if shard.active_by_topic.contains_key(&topic_id) {
                Err(ServerReservationFailure::DuplicateTopicId)
            } else if shard.topic_by_key.contains_key(key) {
                Err(ServerReservationFailure::DuplicateKey)
            } else {
                match self.active_quota.try_acquire() {
                    Ok(permit) => {
                        shard.topic_by_key.insert(key.clone(), topic_id);
                        shard.active_by_topic.insert(
                            topic_id,
                            ActiveSubscription {
                                key: key.clone(),
                                generation: conn_gen,
                                subscription: None,
                                committed: false,
                                latest: StoredRtdValue::Empty,
                                _permit: permit,
                            },
                        );
                        Ok(())
                    }
                    Err(error) => Err(ServerReservationFailure::Overloaded(error)),
                }
            }
        };

        if let Err(failure) = reservation_result {
            self.rollback_catalog_connection_reservation(key, conn_gen);
            return Err(failure.into_xll_error());
        }

        let erased_sink = ErasedSink {
            server: Arc::downgrade(&server_handle.inner),
            topic_id,
            connection_generation: conn_gen,
        };

        let sub_res = catch_unwind(AssertUnwindSafe(|| source.subscribe(&topic, erased_sink)));

        let subscription = match sub_res {
            Ok(Ok(sub)) => sub,
            Ok(Err(err)) => {
                let _ = self.rollback_connection(&server_handle.inner, topic_id, conn_gen, key);
                return Err(err);
            }
            Err(panic_payload) => {
                let _ = self.rollback_connection(&server_handle.inner, topic_id, conn_gen, key);
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::PANIC_SUBSCRIPTION,
                }));
                std::panic::resume_unwind(panic_payload);
            }
        };

        let install_result = {
            let mut shard = server_handle.inner.shards[shard_index].lock();
            if server_handle.inner.ensure_open().is_err() {
                Err(subscription)
            } else {
                match shard.active_by_topic.get_mut(&topic_id) {
                    Some(active) if active.generation == conn_gen => {
                        active.subscription = Some(subscription);
                        let latest = active.latest.clone();
                        let epoch = server_handle.inner.publish_epoch.load(Ordering::Acquire);
                        let buf0 = (epoch & 1) as usize;
                        let buf1 = 1 - buf0;
                        let observed = shard.pending[buf0]
                            .get(&topic_id)
                            .or_else(|| shard.pending[buf1].get(&topic_id))
                            .filter(|u| u.connection_generation == conn_gen)
                            .map(|u| u.sequence);
                        Ok((latest, observed))
                    }
                    _ => Err(subscription),
                }
            }
        };

        let (latest_value, observed_sequence) = match install_result {
            Ok(res) => res,
            Err(sub) => {
                let cleanup_res = disconnect_one_no_unwind(sub);
                let rollback_res =
                    self.rollback_connection(&server_handle.inner, topic_id, conn_gen, key);
                let first_error = cleanup_res.err().or_else(|| rollback_res.err());
                if let Some(error) = first_error {
                    self.record_cleanup_result(Err(error.clone()));
                    return Err(error);
                }
                return Err(XllError::Closing);
            }
        };

        Ok(SubscriptionConnection {
            runtime: Arc::clone(self),
            operation: Some(operation),
            topic_id,
            generation: conn_gen,
            key: key.clone(),
            value: latest_value,
            observed_sequence,
            created: true,
            finished: false,
        })
    }

    pub(crate) fn commit_connection(
        &self,
        server: &Arc<ServerRuntime>,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        key: &SubscriptionKey,
        observed_sequence: Option<u64>,
    ) -> XllResult<()> {
        let attempt = {
            let shard_index = shard_index(topic_id);
            let mut shard = server.shards[shard_index].lock();
            server.ensure_open()?;
            let Some(active) = shard.active_by_topic.get_mut(&topic_id) else {
                return Err(XllError::Closing);
            };
            if active.generation != generation {
                return Err(XllError::Closing);
            }
            active.committed = true;

            if let Some(obs) = observed_sequence {
                if shard.pending[0]
                    .get(&topic_id)
                    .is_some_and(|u| u.sequence <= obs)
                {
                    shard.pending[0].remove(&topic_id);
                    server.pending_updates.fetch_sub(1, Ordering::Relaxed);
                }
                if shard.pending[1]
                    .get(&topic_id)
                    .is_some_and(|u| u.sequence <= obs)
                {
                    shard.pending[1].remove(&topic_id);
                    server.pending_updates.fetch_sub(1, Ordering::Relaxed);
                }
            }

            let epoch = server.publish_epoch.load(Ordering::Acquire);
            let buf0 = (epoch & 1) as usize;
            let buf1 = 1 - buf0;
            let has_pending = shard.pending[buf0]
                .get(&topic_id)
                .or_else(|| shard.pending[buf1].get(&topic_id))
                .is_some_and(|u| {
                    u.connection_generation == generation
                        && observed_sequence.is_none_or(|seq| u.sequence > seq)
                });
            if has_pending {
                let mut refresh = server.refresh.lock();
                let has_updates = server.has_deliverable_updates();
                let prepared = refresh.prepare_notification(has_updates)?;
                prepared.map(|p| {
                    server.notified_epoch.store(epoch, Ordering::Release);
                    refresh.commit_notification(p)
                })
            } else {
                None
            }
        };

        let removed_source = {
            let mut catalog = self.catalog.lock();
            if let Some(binding) = catalog
                .active_keys
                .get_mut(key)
                .filter(|b| b.connection_generation == generation)
            {
                binding.stage = BindingStage::Active;
            }
            if let Some(pending) = catalog
                .pending
                .get_mut(key)
                .filter(|p| p.connecting_generation == Some(generation))
            {
                pending.connecting_generation = None;
            }
            let remove_pending = catalog
                .pending
                .get(key)
                .is_some_and(|p| p.live_reservations == 0 && p.connecting_generation.is_none());

            if remove_pending {
                let removed = catalog.pending.remove(key);
                if let Some(removed) = removed {
                    let bytes = removed.topic.byte_len();
                    catalog.pending_topic_bytes = catalog.pending_topic_bytes.saturating_sub(bytes);
                    remove_identity_if_unbound(&mut catalog, key);
                    Some(removed.source)
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(source) = removed_source {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::PANIC_SOURCE,
                }));
            }
        }

        if let Some(attempt) = attempt {
            server.drive_notification(attempt);
        }

        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddSubscription);

        Ok(())
    }

    pub(crate) fn rollback_connection(
        &self,
        server: &ServerRuntime,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        key: &SubscriptionKey,
    ) -> XllResult<()> {
        let (subscription, _removed_update) = {
            let shard_index = shard_index(topic_id);
            let mut shard = server.shards[shard_index].lock();
            let sub = shard
                .active_by_topic
                .get_mut(&topic_id)
                .filter(|a| a.generation == generation)
                .and_then(|a| a.subscription.take());

            if shard
                .active_by_topic
                .get(&topic_id)
                .is_some_and(|a| a.generation == generation)
            {
                shard.active_by_topic.remove(&topic_id);
            }
            if shard.topic_by_key.get(key).is_some_and(|&tid| {
                shard
                    .active_by_topic
                    .get(&tid)
                    .is_none_or(|a| a.generation == generation)
            }) {
                shard.topic_by_key.remove(key);
            }

            let rem_update = if shard.pending[0]
                .get(&topic_id)
                .is_some_and(|u| u.connection_generation == generation)
            {
                shard.pending[0].remove(&topic_id)
            } else if shard.pending[1]
                .get(&topic_id)
                .is_some_and(|u| u.connection_generation == generation)
            {
                shard.pending[1].remove(&topic_id)
            } else {
                None
            };
            (sub, rem_update)
        };

        let removed_source = {
            let mut catalog = self.catalog.lock();
            if catalog
                .active_keys
                .get(key)
                .is_some_and(|b| b.connection_generation == generation)
            {
                catalog.active_keys.remove(key);
            }

            if let Some(pending) = catalog
                .pending
                .get_mut(key)
                .filter(|p| p.connecting_generation == Some(generation))
            {
                pending.connecting_generation = None;
            }

            let remove_pending = catalog.pending.get(key).is_some_and(|p| {
                p.live_reservations == 0 && !p.committed && p.connecting_generation.is_none()
            });

            if remove_pending {
                let removed = catalog.pending.remove(key);
                if let Some(removed) = removed {
                    let bytes = removed.topic.byte_len();
                    catalog.pending_topic_bytes = catalog.pending_topic_bytes.saturating_sub(bytes);
                    remove_identity_if_unbound(&mut catalog, key);
                    Some(removed.source)
                } else {
                    None
                }
            } else {
                None
            }
        };

        let mut first_error = None;

        if let Some(sub) = subscription {
            let res = disconnect_one_no_unwind(sub);
            if let Err(ref err) = res {
                self.record_cleanup_result(res.clone());
                first_error = Some(err.clone());
            }
        }

        if let Some(source) = removed_source {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                let err = XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::PANIC_SOURCE,
                };
                self.record_cleanup_result(Err(err.clone()));
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn disconnect(
        &self,
        server_handle: &RtdServerHandle,
        topic_id: TopicId,
    ) -> XllResult<()> {
        let (subscription, key_to_clean, conn_gen) = {
            let shard_index = shard_index(topic_id);
            let mut shard = server_handle.inner.shards[shard_index].lock();
            server_handle.inner.ensure_open()?;
            let Some((tid, active)) = shard.active_by_topic.remove_entry(&topic_id) else {
                return Ok(());
            };
            shard.topic_by_key.remove(&active.key);
            shard.pending[0].remove(&tid);
            shard.pending[1].remove(&tid);
            (active.subscription, active.key, active.generation)
        };

        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveSubscription);

        let removed_source = {
            let mut catalog = self.catalog.lock();
            cleanup_catalog_binding_and_pending(
                &mut catalog,
                &key_to_clean,
                server_handle.inner.generation,
                conn_gen,
            )
        };

        let disconnect_result = subscription.map(disconnect_one_no_unwind);
        let mut first_error = disconnect_result.and_then(|res| res.err());

        if let Some(source) = removed_source
            && catch_unwind(AssertUnwindSafe(|| drop(source))).is_err()
            && first_error.is_none()
        {
            first_error = Some(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::PANIC_SOURCE,
            });
        }

        if let Some(ref err) = first_error {
            self.record_cleanup_result(Err(err.clone()));
        }

        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn close(&self) -> XllResult<()> {
        {
            let mut term_state = self.termination_coordinator.state.lock();
            match term_state.phase {
                ServerTerminationPhase::Terminated | ServerTerminationPhase::Failed => {
                    return self.cleanup_result();
                }
                ServerTerminationPhase::Terminating => {
                    while term_state.phase == ServerTerminationPhase::Terminating {
                        self.termination_coordinator.completed.wait(&mut term_state);
                    }
                    return self.cleanup_result();
                }
                ServerTerminationPhase::Open => {
                    term_state.phase = ServerTerminationPhase::Terminating;
                }
            }
        }

        let runtime_wait = self.runtime_gate.close_and_wait_begin();
        runtime_wait.wait();

        let server_handles = {
            let servers = self.servers.lock();
            servers.values().cloned().collect::<Vec<_>>()
        };

        let admissions = server_handles
            .iter()
            .map(ServerRuntime::begin_termination)
            .collect::<Vec<_>>();

        let cancel_results = admissions
            .iter()
            .map(|admission| match admission {
                TerminationAdmission::Owner(owner) => owner.request_cancel(),
                _ => Ok(()),
            })
            .collect::<Vec<_>>();

        let mut first_error = None;
        for ((server, admission), cancel_res) in
            server_handles.iter().zip(admissions).zip(cancel_results)
        {
            let res = match admission {
                TerminationAdmission::Owner(owner) => owner.finish(cancel_res),
                TerminationAdmission::Waiter(waiter) => waiter.wait(),
                TerminationAdmission::Complete => server.termination_result(),
            };
            if let Err(err) = res
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }

        if let Some(err) = first_error {
            self.record_cleanup_result(Err(err));
        }

        let pending_sources = {
            let mut catalog = self.catalog.lock();
            #[cfg(any(test, feature = "shutdown-refinement"))]
            {
                for _ in 0..catalog.active_keys.len() {
                    self.record_ghost_event(
                        crate::shutdown_refinement::GhostEvent::RemoveSubscription,
                    );
                }
            }
            catalog.active_keys.clear();
            catalog.sources.clear();
            catalog.identities.clear();
            catalog.pending_topic_bytes = 0;
            catalog
                .pending
                .drain()
                .map(|(_, pending)| pending.source)
                .collect::<Vec<_>>()
        };

        for source in pending_sources {
            let res = catch_unwind(AssertUnwindSafe(|| drop(source)));
            if res.is_err() {
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::PANIC_SOURCE,
                }));
            }
        }

        {
            let mut term_state = self.termination_coordinator.state.lock();
            term_state.phase = ServerTerminationPhase::Terminated;
            self.termination_coordinator.completed.notify_all();
        }

        self.cleanup_result()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreparationOwnership {
    CreatedPending,
    ExistingPending,
    ExistingActive,
}

#[derive(Debug)]
pub(crate) struct PreparedSubscription {
    pub(crate) runtime: Weak<SubscriptionRuntime>,
    pub(crate) key: SubscriptionKey,
    pub(crate) reservation_id: Option<u64>,
    pub(crate) ownership: PreparationOwnership,
}

impl PreparedSubscription {
    pub(crate) fn key(&self) -> &SubscriptionKey {
        &self.key
    }

    pub(crate) fn commit(mut self) {
        self.finish(true);
    }

    pub(crate) fn rollback(mut self) {
        self.finish(false);
    }

    pub(crate) fn finish(&mut self, committed: bool) {
        if self.ownership == PreparationOwnership::ExistingActive {
            debug_assert!(self.reservation_id.is_none());
            return;
        }
        let Some(reservation_id) = self.reservation_id.take() else {
            return;
        };
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.finish_preparation(&self.key, reservation_id, committed);
        }
    }
}

impl Drop for PreparedSubscription {
    fn drop(&mut self) {
        self.finish(false);
    }
}

pub(crate) struct SubscriptionConnection {
    pub(crate) runtime: Arc<SubscriptionRuntime>,
    pub(crate) operation: Option<OwnedServerOperation>,
    pub(crate) topic_id: TopicId,
    pub(crate) generation: ConnectionGeneration,
    pub(crate) key: SubscriptionKey,
    pub(crate) value: StoredRtdValue,
    pub(crate) observed_sequence: Option<u64>,
    pub(crate) created: bool,
    pub(crate) finished: bool,
}

impl SubscriptionConnection {
    #[inline]
    pub(crate) fn server(&self) -> &Arc<ServerRuntime> {
        &self
            .operation
            .as_ref()
            .expect("active connection operation")
            .server
    }

    pub(crate) fn value(&self) -> &StoredRtdValue {
        &self.value
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        if self.finished {
            return Ok(());
        }
        let result = if self.created {
            self.runtime.commit_connection(
                self.server(),
                self.topic_id,
                self.generation,
                &self.key,
                self.observed_sequence,
            )
        } else {
            Ok(())
        };

        if result.is_ok() {
            self.finished = true;
            self.operation.take();
        }
        result
    }

    pub(crate) fn rollback(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.created
            && let Some(operation) = &self.operation
        {
            let _ = self.runtime.rollback_connection(
                operation.server.as_ref(),
                self.topic_id,
                self.generation,
                &self.key,
            );
        }
        self.operation.take();
    }
}

impl Drop for SubscriptionConnection {
    fn drop(&mut self) {
        self.rollback();
    }
}
