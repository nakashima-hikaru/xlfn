#![allow(
    unsafe_code,
    reason = "subscription runtime resolves stable Box-owned server arena entries through non-owning pointers"
)]

use super::RuntimeServices;
use super::catalog::{PreparationFinish, SubscriptionCatalog, SubscriptionEntry};
use super::data_plane::PublishCore;
use super::delivery::ErasedSink;
use super::host::SubscriptionHost;
use super::identity::allocate_runtime_id;
use super::server::{
    OwnedServerOperation, ServerTerminationPhase, SubscriptionServer, SubscriptionServerHandle,
    TerminationAdmission, TerminationCoordinator, cleanup_catalog_binding_and_pending,
    disconnect_one_no_unwind,
};
use super::source::{RtdSource, RtdSourceHandle, SourceArena};
use super::topic::{
    RtdLimits, RtdTopic, SourceId, SubscriptionId, SubscriptionIdentity, SubscriptionKey, TopicId,
};
use super::value::StoredRtdValue;
use crate::generation::{ConnectionGeneration, RuntimeGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use xlfn_kernel::operation_gate::{OperationGate, OperationGuard};
use xlfn_kernel::quota::Quota;

#[cfg(test)]
pub(crate) type OperationEnterHook = Arc<dyn Fn() + Send + Sync + 'static>;

pub(crate) struct SubscriptionRuntime<H: SubscriptionHost> {
    pub(crate) generation: RuntimeGeneration,
    pub(crate) runtime_id: u64,
    pub(crate) limits: RtdLimits,
    pub(crate) host: H,
    // Pointer-bearing server roots are declared before every field they
    // reference so Rust's declaration-order drop reclaims servers first.
    pub(crate) servers: Mutex<FxHashMap<ServerGeneration, Box<SubscriptionServer<H>>>>,
    pub(crate) catalog: Mutex<SubscriptionCatalog>,
    pub(crate) sources: SourceArena,
    pub(crate) runtime_gate: OperationGate,
    pub(crate) active_quota: Quota,
    pub(crate) queued_update_quota: Quota,
    pub(crate) next_connection_generation: AtomicU64,
    pub(crate) termination_coordinator: TerminationCoordinator,
    pub(crate) services: RuntimeServices,
    #[cfg(test)]
    pub(crate) test_enter_hook: Mutex<Option<OperationEnterHook>>,
}

impl<H: SubscriptionHost> SubscriptionRuntime<H> {
    #[cfg(test)]
    pub(crate) fn new() -> Self
    where
        H: Default,
    {
        Self::with_host(
            RuntimeGeneration::new(1).expect("test generation is non-zero"),
            RtdLimits::standard(),
            H::default(),
            SourceArena::empty(RuntimeGeneration::new(1).expect("test generation is non-zero")),
        )
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn with_sources_for_internal(sources: SourceArena) -> Self
    where
        H: Default,
    {
        let generation = RuntimeGeneration::new(1).expect("test generation is non-zero");
        Self::with_host(generation, RtdLimits::standard(), H::default(), sources)
    }

    pub(crate) fn with_host(
        generation: RuntimeGeneration,
        limits: RtdLimits,
        host: H,
        sources: SourceArena,
    ) -> Self {
        let runtime_id = allocate_runtime_id().expect("runtime ID allocation overflow");
        Self {
            generation,
            runtime_id,
            limits,
            host,
            servers: Mutex::new(FxHashMap::default()),
            catalog: Mutex::new(SubscriptionCatalog {
                entries: FxHashMap::default(),
                pending_topic_bytes: 0,
                identities: super::identity::SubscriptionIdentityIndex::default(),
                next_subscription_id: 1,
            }),
            sources,
            runtime_gate: OperationGate::new(),
            active_quota: Quota::new(limits.max_active.get()),
            queued_update_quota: Quota::new(limits.max_queued_updates.get()),
            next_connection_generation: AtomicU64::new(1),
            termination_coordinator: TerminationCoordinator::default(),
            services: RuntimeServices::new(),
            #[cfg(test)]
            test_enter_hook: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_operation_enter_hook(&self, hook: Option<OperationEnterHook>) {
        *self.test_enter_hook.lock() = hook;
    }

    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.services.set_trace_sink(trace);
    }

    pub(crate) fn record_shutdown_event(&self, event: crate::shutdown_trace::ShutdownEvent) {
        self.services.record(event);
    }

    pub(crate) fn record_cleanup_result(&self, result: XllResult<()>) {
        self.services.record_cleanup_result(result);
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.services.cleanup_result()
    }

    pub(crate) fn enter_external_operation(&self) -> XllResult<OperationGuard<'_>> {
        self.runtime_gate.enter().map_err(|_| XllError::Closing)
    }

    pub(crate) fn register_server(
        &self,
        generation: ServerGeneration,
    ) -> XllResult<SubscriptionServerHandle<H>> {
        let _operation = self.runtime_gate.enter().map_err(|_| XllError::Closing)?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        let publish = Box::new(PublishCore::new(
            self.host.clone(),
            &self.runtime_gate,
            &self.queued_update_quota,
            &self.services,
        ));
        let server = Box::new(SubscriptionServer {
            generation,
            publish,
            subscriptions: Mutex::new(FxHashMap::default()),
            termination_coordinator: TerminationCoordinator::default(),
        });

        let mut servers = self.servers.lock();
        if servers.contains_key(&generation) {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SERVER_DUE,
            });
        }
        servers.insert(generation, server);
        Ok(SubscriptionServerHandle::new(self, generation))
    }

    pub(crate) fn resolve_server(
        &self,
        generation: ServerGeneration,
    ) -> Option<&SubscriptionServer<H>> {
        let pointer = {
            let servers = self.servers.lock();
            NonNull::from(servers.get(&generation)?.as_ref())
        };
        // SAFETY: server entries are retained as tombstones until the runtime
        // itself is reclaimed; the returned borrow is tied to `&self`.
        Some(unsafe { pointer.as_ref() })
    }

    pub(crate) fn prepare<S>(
        &self,
        source: &RtdSourceHandle<S>,
        topic: RtdTopic,
    ) -> XllResult<PreparedSubscription<'_, H>>
    where
        S: RtdSource,
    {
        let _operation = self.runtime_gate.enter().map_err(|_| XllError::Closing)?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        if source.id.generation != self.generation {
            return Err(XllError::StaleHandle);
        }
        let mut catalog = self.catalog.lock();

        let identity = SubscriptionIdentity {
            source_id: SourceId(source.id),
            topic: topic.clone(),
        };

        if let Some(existing_id) = catalog.identities.get_id(&identity) {
            let existing_key = SubscriptionKey::from_internal(self.runtime_id, existing_id);
            let entry = catalog
                .entries
                .get(&existing_id)
                .ok_or(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_ORPHAN,
                })?;

            if entry.is_connected() {
                return Ok(PreparedSubscription {
                    id: existing_id,
                    key: existing_key,
                    reservation: None,
                });
            }

            if catalog.entries.contains_key(&existing_id) {
                let Some(result) =
                    catalog.with_entry(existing_id, SubscriptionEntry::add_reservation)
                else {
                    return Err(XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_ORPHAN,
                    });
                };
                result?;

                return Ok(PreparedSubscription {
                    id: existing_id,
                    key: existing_key,
                    reservation: Some(PreparationReservation { runtime: self }),
                });
            }

            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_ORPHAN,
            });
        }

        let (id, key) = catalog.insert_pending(self.runtime_id, source.id, topic, self.limits)?;

        Ok(PreparedSubscription {
            id,
            key,
            reservation: Some(PreparationReservation { runtime: self }),
        })
    }

    pub(crate) fn finish_preparation(&self, id: SubscriptionId, committed: bool) {
        {
            let mut catalog = self.catalog.lock();
            let Some(finish) = catalog.with_entry(id, |entry| entry.finish_preparation(committed))
            else {
                return;
            };

            match finish {
                PreparationFinish::Remove => {
                    catalog.remove_entry(id);
                }
                PreparationFinish::Keep => {}
            }
        }
    }

    pub(crate) fn resolve_transport_key(&self, key: SubscriptionKey) -> XllResult<SubscriptionId> {
        key.validate_runtime(self.runtime_id)
            .ok_or(XllError::StaleHandle)
    }

    pub(crate) fn claim_server(
        &self,
        generation: ServerGeneration,
        id: SubscriptionId,
    ) -> XllResult<()> {
        let mut catalog = self.catalog.lock();
        catalog
            .with_entry(id, |entry| entry.claim_server(generation))
            .ok_or(XllError::Closing)??;
        Ok(())
    }

    pub(crate) fn rollback_catalog_connection_reservation(
        &self,
        id: SubscriptionId,
        generation: ConnectionGeneration,
    ) {
        let mut catalog = self.catalog.lock();

        let should_remove = catalog
            .with_entry(id, |entry| {
                entry.rollback_connection(generation);
                entry.can_remove()
            })
            .unwrap_or(false);

        if should_remove {
            catalog.remove_entry(id);
        }
    }

    pub(crate) fn connect_transaction(
        &self,
        server_handle: &SubscriptionServerHandle<H>,
        topic_id: TopicId,
        id: SubscriptionId,
    ) -> XllResult<SubscriptionConnection<H>> {
        let server = server_handle.server()?;
        let operation = server.enter_owned_operation()?;
        let conn_gen = ConnectionGeneration::new(
            self.next_connection_generation
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    current.checked_add(1)
                })
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
                })?,
        )
        .ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
        })?;

        let (source_id, topic) = {
            let mut catalog = self.catalog.lock();
            let Some(result) = catalog.with_entry(id, |entry| -> XllResult<_> {
                entry.begin_connection(server.generation, conn_gen)?;
                Ok((entry.source_id.0, entry.topic.clone()))
            }) else {
                return Err(XllError::Closing);
            };
            result?
        };
        let source = self
            .sources
            .resolve(source_id)
            .ok_or(XllError::StaleHandle)?;

        if let Err(error) =
            server
                .publish
                .reserve_connection(topic_id, id, conn_gen, &self.active_quota)
        {
            self.rollback_catalog_connection_reservation(id, conn_gen);
            return Err(error);
        }

        let erased_sink = ErasedSink::for_publish(server.publish.as_ref(), topic_id, conn_gen);

        let sub_res = catch_unwind(AssertUnwindSafe(|| source.subscribe(&topic, erased_sink)));

        let subscription = match sub_res {
            Ok(Ok(sub)) => sub,
            Ok(Err(err)) => {
                let _ = self.rollback_connection(server, topic_id, conn_gen, id);
                return Err(err);
            }
            Err(panic_payload) => {
                let _ = self.rollback_connection(server, topic_id, conn_gen, id);
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::PANIC_SUBSCRIPTION,
                }));
                std::panic::resume_unwind(panic_payload);
            }
        };

        let install_result = match server.publish.install_connection(topic_id, conn_gen) {
            Ok(installed) => {
                server.subscriptions.lock().insert(topic_id, subscription);
                Ok((installed.latest, installed.observed_sequence))
            }
            Err(_) => Err(subscription),
        };

        let (latest_value, observed_sequence) = match install_result {
            Ok(res) => res,
            Err(sub) => {
                let cleanup_res = disconnect_one_no_unwind(sub);
                let rollback_res = self.rollback_connection(server, topic_id, conn_gen, id);
                let first_error = cleanup_res.err().or_else(|| rollback_res.err());
                if let Some(error) = first_error {
                    self.record_cleanup_result(Err(error.clone()));
                    return Err(error);
                }
                return Err(XllError::Closing);
            }
        };

        Ok(SubscriptionConnection {
            runtime: NonNull::from(self),
            operation: Some(operation),
            topic_id,
            generation: conn_gen,
            id,
            value: latest_value,
            observed_sequence,
            created: true,
            finished: false,
        })
    }

    pub(crate) fn commit_connection(
        &self,
        server: &SubscriptionServer<H>,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        id: SubscriptionId,
        observed_sequence: Option<u64>,
    ) -> XllResult<()> {
        let attempt = server
            .publish
            .commit_connection(topic_id, generation, observed_sequence)?;

        {
            let mut catalog = self.catalog.lock();
            if !catalog
                .with_entry(id, |entry| entry.finish_connection(generation))
                .unwrap_or(false)
            {
                return Err(XllError::Closing);
            }
        }

        if let Some(attempt) = attempt {
            server.publish.drive_notification(attempt);
        }

        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::AddSubscription);

        Ok(())
    }

    pub(crate) fn rollback_connection(
        &self,
        server: &SubscriptionServer<H>,
        topic_id: TopicId,
        generation: ConnectionGeneration,
        id: SubscriptionId,
    ) -> XllResult<()> {
        let subscription = server.subscriptions.lock().remove(&topic_id);
        server.publish.rollback_connection(topic_id, generation, id);

        {
            let mut catalog = self.catalog.lock();
            let transitioned = catalog
                .with_entry(id, |entry| entry.rollback_connection(generation))
                .unwrap_or(false);

            if transitioned
                && catalog
                    .entries
                    .get(&id)
                    .is_some_and(SubscriptionEntry::can_remove)
            {
                catalog.remove_entry(id);
            }
        }

        let mut first_error = None;

        if let Some(sub) = subscription {
            let res = disconnect_one_no_unwind(sub);
            if let Err(ref err) = res {
                self.record_cleanup_result(res.clone());
                first_error = Some(err.clone());
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn disconnect(
        &self,
        server_handle: &SubscriptionServerHandle<H>,
        topic_id: TopicId,
    ) -> XllResult<()> {
        let server = server_handle.server()?;
        let _operation = server.enter_operation()?;
        let subscription = server.subscriptions.lock().remove(&topic_id);
        let Some(retired) = server.publish.disconnect_connection(topic_id)? else {
            return Ok(());
        };
        let id_to_clean = retired.id;
        let conn_gen = retired.generation;

        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveSubscription);

        {
            let mut catalog = self.catalog.lock();
            cleanup_catalog_binding_and_pending(
                &mut catalog,
                id_to_clean,
                server.generation,
                conn_gen,
            );
        }

        let disconnect_result = subscription.map(disconnect_one_no_unwind);
        let first_error = disconnect_result.and_then(|res| res.err());

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

        let server_pointers = {
            let servers = self.servers.lock();
            servers
                .values()
                .map(|server| NonNull::from(server.as_ref()))
                .collect::<Vec<_>>()
        };

        let admissions = server_pointers
            .iter()
            .map(|pointer| {
                // SAFETY: servers are retained in the runtime arena until
                // this close operation completes and the runtime is dropped.
                unsafe { pointer.as_ref() }.begin_termination(self)
            })
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
            server_pointers.iter().zip(admissions).zip(cancel_results)
        {
            // SAFETY: same arena-retention proof as admission creation.
            let server = unsafe { server.as_ref() };
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

        {
            let mut catalog = self.catalog.lock();
            for _ in 0..catalog
                .entries
                .values()
                .filter(|entry| entry.is_connected())
                .count()
            {
                self.record_shutdown_event(
                    crate::shutdown_trace::ShutdownEvent::RemoveSubscription,
                );
            }
            catalog.identities.clear();
            catalog.pending_topic_bytes = 0;
            catalog.entries.clear();
        }

        {
            let mut term_state = self.termination_coordinator.state.lock();
            term_state.phase = ServerTerminationPhase::Terminated;
            self.termination_coordinator.completed.notify_all();
        }

        self.cleanup_result()
    }

    pub(crate) fn terminate_server(&self, generation: ServerGeneration) -> XllResult<()> {
        let server = self.resolve_server(generation).ok_or(XllError::Closing)?;
        match server.begin_termination(self) {
            TerminationAdmission::Owner(owner) => {
                let cancel_result = owner.request_cancel();
                owner.finish(cancel_result)
            }
            TerminationAdmission::Waiter(waiter) => waiter.wait(),
            TerminationAdmission::Complete => server.termination_result(),
        }
    }
}

struct PreparationReservation<'runtime, H: SubscriptionHost> {
    runtime: &'runtime SubscriptionRuntime<H>,
}

pub(crate) struct PreparedSubscription<'runtime, H: SubscriptionHost> {
    id: SubscriptionId,
    key: SubscriptionKey,
    reservation: Option<PreparationReservation<'runtime, H>>,
}

impl<H: SubscriptionHost> PreparedSubscription<'_, H> {
    #[cfg(any(test, all(feature = "bench-internals", feature = "rtd")))]
    #[inline]
    pub(crate) fn id(&self) -> SubscriptionId {
        self.id
    }

    #[inline]
    pub(crate) fn key(&self) -> &SubscriptionKey {
        &self.key
    }

    pub(crate) fn commit(mut self) {
        self.finish(true);
    }

    pub(crate) fn rollback(mut self) {
        self.finish(false);
    }

    fn finish(&mut self, committed: bool) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        reservation.runtime.finish_preparation(self.id, committed);
    }

    #[cfg(test)]
    pub(crate) fn has_reservation(&self) -> bool {
        self.reservation.is_some()
    }
}

impl<H: SubscriptionHost> Drop for PreparedSubscription<'_, H> {
    fn drop(&mut self) {
        self.finish(false);
    }
}

pub(crate) struct SubscriptionConnection<H: SubscriptionHost> {
    pub(crate) runtime: NonNull<SubscriptionRuntime<H>>,
    pub(crate) operation: Option<OwnedServerOperation<H>>,
    pub(crate) topic_id: TopicId,
    pub(crate) generation: ConnectionGeneration,
    pub(crate) id: SubscriptionId,
    pub(crate) value: StoredRtdValue,
    pub(crate) observed_sequence: Option<u64>,
    pub(crate) created: bool,
    pub(crate) finished: bool,
}

// SAFETY: the connection carries an OwnedServerOperation with runtime/server
// operation permits, ensuring liveness across threads until finished or dropped.
unsafe impl<H: SubscriptionHost> Send for SubscriptionConnection<H> {}

impl<H: SubscriptionHost> SubscriptionConnection<H> {
    #[inline]
    fn runtime(&self) -> &SubscriptionRuntime<H> {
        // SAFETY: the owned operation contains a runtime-gate permit, so the
        // runtime cannot be reclaimed before the connection completes.
        unsafe { self.runtime.as_ref() }
    }

    #[inline]
    pub(crate) fn server(&self) -> &SubscriptionServer<H> {
        self.operation
            .as_ref()
            .expect("active connection operation")
            .server()
    }

    pub(crate) fn value(&self) -> &StoredRtdValue {
        &self.value
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        if self.finished {
            return Ok(());
        }
        let result = if self.created {
            self.runtime().commit_connection(
                self.server(),
                self.topic_id,
                self.generation,
                self.id,
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
            let _ = self.runtime().rollback_connection(
                operation.server(),
                self.topic_id,
                self.generation,
                self.id,
            );
        }
        self.operation.take();
    }
}

impl<H: SubscriptionHost> Drop for SubscriptionConnection<H> {
    fn drop(&mut self) {
        self.rollback();
    }
}

impl<H: SubscriptionHost> Drop for SubscriptionRuntime<H> {
    fn drop(&mut self) {
        self.runtime_gate.begin_close();
    }
}
