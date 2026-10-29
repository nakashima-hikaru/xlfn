#![allow(
    unsafe_code,
    reason = "subscription runtime resolves stable Box-owned server arena entries through non-owning pointers"
)]

use super::RuntimeServices;
use super::catalog::{ConnectionTicket, PreparationFinish, SubscriptionCatalog, SubscriptionEntry};
use super::data_plane::PublishCore;
use super::disconnect::OwnedSubscription;
use super::host::SubscriptionHost;
use super::identity::allocate_runtime_id;
use super::server::{
    OwnedServerOperation, SubscriptionServer, SubscriptionServerHandle, TerminationAdmission,
    cleanup_catalog_binding_and_pending,
};
use super::source::{RtdSourceHandle, RtdSubscription, SourceArena, SourceSubscription};
use super::termination::{TerminationClaim, TerminationCoordinator};
use super::topic::{
    BorrowedTopicParts, RtdLimits, SourceId, SubscriptionId, SubscriptionKey, TopicId,
};
use super::value::StoredRtdValue;
use crate::generation::{ConnectionGeneration, RuntimeGeneration, ServerGeneration};
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use xlfn_kernel::operation_gate::OperationGuard;
use xlfn_kernel::published_owner::PublishedOwner;

#[cfg(test)]
pub(crate) type OperationEnterHook = Arc<dyn Fn() + Send + Sync + 'static>;

type ServerMap<H, E, C> = FxHashMap<ServerGeneration, PublishedOwner<SubscriptionServer<H, E, C>>>;

pub(crate) struct SubscriptionRuntime<
    H: SubscriptionHost,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    pub(crate) generation: RuntimeGeneration,
    pub(crate) runtime_id: u64,
    pub(crate) limits: RtdLimits,
    // Deferred jobs carry non-owning admissions into the following server and
    // service allocations. Reclaim their worker before those allocations even
    // if unwinding interrupts the explicit close sequence.
    pub(super) disconnects: super::disconnect::DisconnectPool<H, E, C>,
    pub(crate) host: H,
    // Pointer-bearing server roots are declared before every field they
    // reference so Rust's declaration-order drop reclaims servers first.
    pub(crate) servers: Mutex<ServerMap<H, E, C>>,
    pub(crate) catalog: Mutex<SubscriptionCatalog>,
    pub(crate) sources: SourceArena<E, C>,
    pub(crate) next_connection_generation: AtomicU64,
    pub(super) termination_coordinator: TerminationCoordinator,
    // Every raw publish capability targets this independent allocation, so
    // final Drop can drain producers without invalidating their references.
    pub(crate) services: PublishedOwner<RuntimeServices>,
    #[cfg(test)]
    pub(crate) test_enter_hook: Mutex<Option<OperationEnterHook>>,
    #[cfg(test)]
    pub(crate) fail_source_shutdown_preflight: std::sync::atomic::AtomicBool,
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    SubscriptionRuntime<H, E, C>
{
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
    pub(crate) fn with_sources_for_internal(sources: SourceArena<E, C>) -> Self
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
        sources: SourceArena<E, C>,
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
                pending_count: 0,
                identities: super::identity::SubscriptionIdentityIndex::default(),
                next_subscription_id: 1,
            }),
            sources,
            next_connection_generation: AtomicU64::new(1),
            termination_coordinator: TerminationCoordinator::default(),
            services: PublishedOwner::new(RuntimeServices::new(limits)),
            disconnects: super::disconnect::DisconnectPool::new(limits.max_active.get()),
            #[cfg(test)]
            test_enter_hook: Mutex::new(None),
            #[cfg(test)]
            fail_source_shutdown_preflight: std::sync::atomic::AtomicBool::new(false),
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
        self.services
            .runtime_gate
            .enter()
            .map_err(|_| XllError::Closing)
    }

    /// Registers a server generation and returns a server handle.
    ///
    /// # Safety
    ///
    /// Keep `self` at a stable address through every method call on the returned
    /// handle and its copies, including every guard or batch they return. Drain
    /// those operations before reclaiming the runtime. Inert copies may remain
    /// afterward, but must never be used again; dropping a handle dereferences
    /// neither the runtime nor the server.
    pub(crate) unsafe fn register_server(
        &self,
        generation: ServerGeneration,
    ) -> XllResult<SubscriptionServerHandle<H, E, C>> {
        let _operation = self
            .services
            .runtime_gate
            .enter()
            .map_err(|_| XllError::Closing)?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        // SAFETY: all referenced fields are owned by this runtime, and the
        // declaration order keeps every server/core alive before those fields
        // are reclaimed. `register_server` also carries the runtime lifetime
        // contract for the returned handle.
        let publish = xlfn_kernel::published_owner::PublishedOwner::new(unsafe {
            PublishCore::new(self.host.clone(), &self.services)
        });
        let server = xlfn_kernel::published_owner::PublishedOwner::new(SubscriptionServer {
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
        let pointer = NonNull::from(server.as_ref());
        servers.insert(generation, server);
        // SAFETY: the caller retains the stable runtime through every handle use;
        // shutdown drains all operations before runtime reclamation.
        Ok(unsafe { SubscriptionServerHandle::new(self, pointer) })
    }

    #[cfg(all(test, feature = "rtd"))]
    pub(crate) fn register_test_server(
        &self,
        generation: u64,
    ) -> SubscriptionServerHandle<H, E, C> {
        // SAFETY: test helper guarantees runtime outlives the server handle in the test.
        unsafe {
            self.register_server(
                ServerGeneration::new(generation).expect("non-zero test server generation"),
            )
            .unwrap()
        }
    }

    pub(crate) fn resolve_server(
        &self,
        generation: ServerGeneration,
    ) -> Option<&SubscriptionServer<H, E, C>> {
        let pointer = {
            let servers = self.servers.lock();
            NonNull::from(servers.get(&generation)?.as_ref())
        };
        // SAFETY: server entries are retained as tombstones until the runtime
        // itself is reclaimed; the returned borrow is tied to `&self`.
        Some(unsafe { pointer.as_ref() })
    }

    pub(crate) fn prepare<S, Part: AsRef<str>>(
        &self,
        source: &RtdSourceHandle<S>,
        topic: BorrowedTopicParts<'_, Part>,
    ) -> XllResult<PreparedSubscription<'_, H, E, C>> {
        let _operation = self
            .services
            .runtime_gate
            .enter()
            .map_err(|_| XllError::Closing)?;
        #[cfg(test)]
        if let Some(hook) = self.test_enter_hook.lock().as_ref().cloned() {
            hook();
        }
        if source.id.generation != self.generation || self.sources.resolve(source.id).is_none() {
            return Err(XllError::StaleHandle);
        }
        let mut catalog = self.catalog.lock();

        if let Some((existing_id, entry)) = catalog.find_identity(SourceId(source.id), &topic)? {
            let existing_key = SubscriptionKey::from_internal(self.runtime_id, existing_id);
            if entry.is_connected() {
                return Ok(PreparedSubscription {
                    id: existing_id,
                    key: existing_key,
                    reservation: None,
                });
            }

            catalog
                .with_entry(existing_id, SubscriptionEntry::add_reservation)
                .ok_or(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_INDEX_ORPHAN,
                })??;

            return Ok(PreparedSubscription {
                id: existing_id,
                key: existing_key,
                reservation: Some(PreparationReservation { runtime: self }),
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

    fn rollback_catalog_connection_reservation(
        &self,
        id: SubscriptionId,
        ticket: &ConnectionTicket,
    ) {
        let mut catalog = self.catalog.lock();

        let should_remove = catalog
            .with_entry(id, |entry| {
                entry.rollback_connection(ticket);
                entry.can_remove()
            })
            .unwrap_or(false);

        if should_remove {
            catalog.remove_entry(id);
        }
    }

    pub(crate) fn connect_transaction(
        &self,
        server_handle: &SubscriptionServerHandle<H, E, C>,
        topic_id: TopicId,
        id: SubscriptionId,
    ) -> XllResult<SubscriptionConnection<H, E, C>> {
        if !server_handle.belongs_to(self) {
            return Err(XllError::StaleHandle);
        }
        let server = server_handle.server()?;
        // SAFETY: the exact handle/runtime identity was checked above; the
        // runtime and its server arena remain allocated for this owned
        // connection operation under the register/close lifecycle contract.
        let operation = unsafe { server.enter_owned_operation()? };
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

        let (source, topic, ticket) = {
            let mut catalog = self.catalog.lock();
            let Some(result) = catalog.with_entry(id, |entry| -> XllResult<_> {
                // Resolve the immutable arena entry before acquiring any
                // catalog connection obligation. A rejected source must leave
                // the pending subscription available for rollback or retry.
                let source = self
                    .sources
                    .resolve(entry.source_id.0)
                    .ok_or(XllError::StaleHandle)?;
                let topic = entry.topic.clone();
                let ticket = entry.begin_connection(server.generation, conn_gen)?;
                Ok((source, topic, ticket))
            }) else {
                return Err(XllError::Closing);
            };
            result?
        };
        let transaction = ConnectionTransaction {
            reservation: Some(ConnectionReservation {
                runtime: NonNull::from(self),
                operation,
                topic_id,
                id,
                ticket,
            }),
            state: CatalogReserved,
        }
        .reserve_publication()?;

        // Prepare worker startup and terminal cleanup capacity before any
        // source receives a sink. Existing owners keep this capacity until
        // their sink-user barrier finishes, including deferred disconnection.
        let cleanup = self.disconnects.reserve()?;

        let publish_sink = H::publish_sink(server.publish.as_ref(), topic_id, conn_gen);

        let sub_res = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: transaction holds server/runtime admission. On success
            // the owner is installed or synchronously disconnected before it
            // releases admission. Deferred cleanup retains its own admission;
            // runtime drop drains servers before reclaiming the source arena.
            unsafe { SourceSubscription::subscribe(&source, &topic, publish_sink) }
        }));

        let subscription = match sub_res {
            Ok(Ok(sub)) => OwnedSubscription::new(sub, cleanup),
            Ok(Err(err)) => return Err(err),
            Err(panic_payload) => {
                let _ = transaction.rollback_with_result();
                self.record_cleanup_result(Err(XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::PANIC_SUBSCRIPTION,
                }));
                std::panic::resume_unwind(panic_payload);
            }
        };

        transaction.install(subscription)
    }

    fn commit_connection(
        &self,
        server: &SubscriptionServer<H, E, C>,
        topic_id: TopicId,
        ticket: &ConnectionTicket,
        id: SubscriptionId,
        observed_sequence: Option<u64>,
    ) -> XllResult<()> {
        let generation = ticket.generation();
        let attempt = server
            .publish
            .commit_connection(topic_id, generation, observed_sequence)?;

        {
            let mut catalog = self.catalog.lock();
            if !catalog
                .with_entry(id, |entry| entry.finish_connection(ticket))
                .unwrap_or(false)
            {
                return Err(XllError::Closing);
            }
        }

        if let Some(attempt) = attempt {
            server.publish.drive_notification(attempt);
        }

        Ok(())
    }

    fn rollback_connection(
        &self,
        server: &SubscriptionServer<H, E, C>,
        topic_id: TopicId,
        ticket: &ConnectionTicket,
        id: SubscriptionId,
    ) -> XllResult<()> {
        let generation = ticket.generation();
        let subscription = {
            let mut subscriptions = server.subscriptions.lock();
            let subscription = server
                .publish
                .rollback_connection(topic_id, generation, id)
                .then(|| subscriptions.remove(&topic_id))
                .flatten();
            if subscription.is_some() {
                self.record_shutdown_event(
                    crate::shutdown_trace::ShutdownEvent::RemoveSubscription,
                );
            }
            subscription
        };

        {
            let mut catalog = self.catalog.lock();
            let transitioned = catalog
                .with_entry(id, |entry| entry.rollback_connection(ticket))
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
            let res = sub.disconnect_and_wait();
            if let Err(ref err) = res {
                self.record_cleanup_result(res.clone());
                first_error = Some(err.clone());
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    #[cfg_attr(
        windows,
        allow(
            dead_code,
            reason = "synchronous backend disconnection retains its sink-user barrier contract; COM uses the deferred entry point"
        )
    )]
    pub(crate) fn disconnect(
        &self,
        server_handle: &SubscriptionServerHandle<H, E, C>,
        topic_id: TopicId,
    ) -> XllResult<()> {
        if !server_handle.belongs_to(self) {
            return Err(XllError::StaleHandle);
        }
        let server = server_handle.server()?;
        let _operation = server.enter_operation()?;
        #[cfg(test)]
        {
            let hook = self.test_enter_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let (retired, subscription) = {
            let mut subscriptions = server.subscriptions.lock();
            let Some(retired) = server.publish.disconnect_connection(topic_id)? else {
                return Ok(());
            };
            let subscription = subscriptions.remove(&topic_id);
            if subscription.is_some() {
                self.record_shutdown_event(
                    crate::shutdown_trace::ShutdownEvent::RemoveSubscription,
                );
            }
            (retired, subscription)
        };
        let id_to_clean = retired.id;
        let conn_gen = retired.generation;

        {
            let mut catalog = self.catalog.lock();
            cleanup_catalog_binding_and_pending(
                &mut catalog,
                id_to_clean,
                server.generation,
                conn_gen,
            );
        }

        let disconnect_result = subscription.map(OwnedSubscription::disconnect_and_wait);
        let first_error = disconnect_result.and_then(|res| res.err());

        if let Some(ref err) = first_error {
            self.record_cleanup_result(Err(err.clone()));
        }

        first_error.map_or(Ok(()), Err)
    }

    /// COM-only disconnection: revoke the connection before returning, then
    /// join its sink users on an owned worker so the caller's STA can dispatch
    /// an outstanding UpdateNotify or a producer's COM call. Synchronous
    /// backend disconnection keeps its existing barrier contract.
    pub(crate) fn disconnect_deferred(
        &self,
        server_handle: &SubscriptionServerHandle<H, E, C>,
        topic_id: TopicId,
        completion: H::DisconnectCompletion,
    ) -> XllResult<()> {
        if !server_handle.belongs_to(self) {
            return Err(XllError::StaleHandle);
        }
        let server = server_handle.server()?;
        // SAFETY: the COM caller owns runtime admission while transferring
        // this operation to the generation-owned queue. Close drains that
        // queue's admissions and joins the worker before reclaiming servers.
        let operation = unsafe { server.enter_owned_operation() }?;
        let mut subscriptions = server.subscriptions.lock();
        if !subscriptions.contains_key(&topic_id) {
            return Ok(());
        }
        // The admitted owner already holds worker startup and queue capacity;
        // terminal disconnection cannot be rejected by resource admission.
        let Some(retired) = server.publish.disconnect_connection(topic_id)? else {
            return Ok(());
        };
        let subscription = subscriptions
            .remove(&topic_id)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        drop(subscriptions);
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveSubscription);
        {
            let mut catalog = self.catalog.lock();
            cleanup_catalog_binding_and_pending(
                &mut catalog,
                retired.id,
                server.generation,
                retired.generation,
            );
        }
        subscription.defer(completion, operation);
        Ok(())
    }

    pub(crate) fn close(&self) -> XllResult<()> {
        let owner = match self.termination_coordinator.begin() {
            TerminationClaim::Owner(owner) => owner,
            TerminationClaim::Waiter(waiter) => return waiter.wait(),
            TerminationClaim::Complete(completed) => return completed.result(),
        };

        let runtime_wait = self.services.runtime_gate.close_and_wait_begin();

        // Shared source workers are independent of individual subscriptions.
        // Cancel every source before waiting for admission: a deferred
        // disconnect may hold admission while waiting for shared work to stop.
        // In-flight setup must synchronize with this cancellation and roll
        // back when necessary. A panic must not skip other source callbacks.
        self.record_cleanup_result(self.sources.request_stop());
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

        // Consuming transitions retain each cancellation result with its owner.
        // Complete every notification before waiting for any server.
        let canceled = admissions
            .into_iter()
            .map(TerminationAdmission::request_cancel)
            .collect::<Vec<_>>();

        let mut first_error = None;
        for admission in canceled {
            let res = admission.finish();
            if let Err(err) = res
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }

        if let Some(err) = first_error {
            self.record_cleanup_result(Err(err));
        }

        // Every subscription has relinquished its raw sink. Join the shared
        // source jobs before any source/service/module can be reclaimed. On
        // Windows keep Excel's STA dispatchable while a source finishes COM
        // work. Preparation failure leaves all join owners in their sources.
        match self.join_sources_with_dispatch() {
            Ok(result) => self.record_cleanup_result(result),
            Err(error) => {
                self.record_cleanup_result(Err(error));
                return owner.fail(self.cleanup_result().expect_err("cleanup failure recorded"));
            }
        }

        // Deferred jobs have drained admission and live owners have now
        // finished their barriers. Join the worker's COM apartment teardown
        // before any server, source, service, or module can be reclaimed.
        if let Err(error) = self.disconnects.stop() {
            // The dispatch preflight did not execute the worker join. Keep
            // the runtime's worker owner intact for caller quarantine, and
            // wake close waiters with the same terminal cleanup failure.
            self.record_cleanup_result(Err(error));
            return owner.fail(self.cleanup_result().expect_err("cleanup failure recorded"));
        }

        #[cfg(feature = "rtd")]
        self.services.publishers.stop();

        {
            let mut catalog = self.catalog.lock();
            catalog.identities.clear();
            catalog.pending_topic_bytes = 0;
            catalog.pending_count = 0;
            catalog.entries.clear();
        }

        owner.complete(self.cleanup_result())
    }

    /// The outer result distinguishes failed dispatch preparation (no hook
    /// executed) from source errors after every shutdown barrier completed.
    fn join_sources_with_dispatch(&self) -> XllResult<XllResult<()>> {
        if self.sources.is_empty() {
            return Ok(Ok(()));
        }
        #[cfg(test)]
        if self
            .fail_source_shutdown_preflight
            .swap(false, Ordering::Relaxed)
        {
            return Err(XllError::Overloaded);
        }
        let mut source_result = Ok(());
        let sources = &self.sources;
        let join_sources = || source_result = sources.shutdown_and_wait();
        #[cfg(all(windows, any(feature = "rtd", feature = "handles")))]
        crate::excel_rtd::drain_with_com_dispatch(join_sources)
            .map_err(|(error, _unused_join)| error)?;
        #[cfg(not(all(windows, any(feature = "rtd", feature = "handles"))))]
        {
            let mut join_sources = join_sources;
            join_sources();
        }
        Ok(source_result)
    }

    pub(crate) fn terminate_server(&self, generation: ServerGeneration) -> XllResult<()> {
        let server = self.resolve_server(generation).ok_or(XllError::Closing)?;
        server.begin_termination(self).request_cancel().finish()
    }
}

struct PreparationReservation<
    'runtime,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
> {
    runtime: &'runtime SubscriptionRuntime<H, E, C>,
}

pub(crate) struct PreparedSubscription<
    'runtime,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
> {
    id: SubscriptionId,
    key: SubscriptionKey,
    reservation: Option<PreparationReservation<'runtime, H, E, C>>,
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    PreparedSubscription<'_, H, E, C>
{
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

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop
    for PreparedSubscription<'_, H, E, C>
{
    fn drop(&mut self) {
        self.finish(false);
    }
}

/// A connection can reach commit only after catalog admission, publication
/// reservation, source subscription, and installation have all succeeded.
/// Each transition consumes its predecessor; every incomplete stage rolls back.
pub(crate) type SubscriptionConnection<H, E = (), C = crate::DynamicRtdSources<E>> =
    ConnectionTransaction<H, E, C, InstalledConnection>;

pub(crate) trait ConnectionStage {
    const HAS_PUBLICATION: bool;
}

struct CatalogReserved;
struct PublishReserved;
pub(crate) struct InstalledConnection {
    value: StoredRtdValue,
    observed_sequence: Option<u64>,
}

impl ConnectionStage for CatalogReserved {
    const HAS_PUBLICATION: bool = false;
}
impl ConnectionStage for PublishReserved {
    const HAS_PUBLICATION: bool = true;
}
impl ConnectionStage for InstalledConnection {
    const HAS_PUBLICATION: bool = true;
}

struct ConnectionReservation<
    H: SubscriptionHost,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    runtime: NonNull<SubscriptionRuntime<H, E, C>>,
    operation: OwnedServerOperation<H, E, C>,
    topic_id: TopicId,
    id: SubscriptionId,
    ticket: ConnectionTicket,
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ConnectionReservation<H, E, C>
{
    fn runtime(&self) -> &SubscriptionRuntime<H, E, C> {
        // SAFETY: the owned operation retains the runtime and server until
        // the transaction finishes, including all rollback callbacks.
        unsafe { self.runtime.as_ref() }
    }

    fn rollback(self, published: bool) -> XllResult<()> {
        if published {
            self.runtime().rollback_connection(
                self.operation.server(),
                self.topic_id,
                &self.ticket,
                self.id,
            )
        } else {
            self.runtime()
                .rollback_catalog_connection_reservation(self.id, &self.ticket);
            Ok(())
        }
    }
}

#[must_use = "dropping a connection transaction rolls it back"]
pub(crate) struct ConnectionTransaction<
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
    S: ConnectionStage,
> {
    // None exists only while a consuming transition or completion transfers
    // the sole obligation; callers cannot construct a disarmed transaction.
    reservation: Option<ConnectionReservation<H, E, C>>,
    state: S,
}

// SAFETY: the owned operation retains runtime/server allocations across threads;
// stage data must itself be transferable.
unsafe impl<
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
    S: ConnectionStage + Send,
> Send for ConnectionTransaction<H, E, C, S>
{
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>, S: ConnectionStage>
    ConnectionTransaction<H, E, C, S>
{
    fn transition<N: ConnectionStage>(mut self, state: N) -> ConnectionTransaction<H, E, C, N> {
        ConnectionTransaction {
            reservation: self.reservation.take(),
            state,
        }
    }

    fn reservation(&self) -> &ConnectionReservation<H, E, C> {
        self.reservation
            .as_ref()
            .expect("live connection transaction")
    }

    fn rollback_with_result(mut self) -> XllResult<()> {
        self.reservation
            .take()
            .expect("live connection transaction")
            .rollback(S::HAS_PUBLICATION)
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ConnectionTransaction<H, E, C, CatalogReserved>
{
    fn reserve_publication(self) -> XllResult<ConnectionTransaction<H, E, C, PublishReserved>> {
        let reservation = self.reservation();
        reservation.operation.server().publish.reserve_connection(
            reservation.topic_id,
            reservation.id,
            reservation.ticket.generation(),
        )?;
        Ok(self.transition(PublishReserved))
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ConnectionTransaction<H, E, C, PublishReserved>
{
    fn install(
        self,
        subscription: OwnedSubscription<H, E, C>,
    ) -> XllResult<SubscriptionConnection<H, E, C>> {
        let reservation = self.reservation();
        let server = reservation.operation.server();
        let install_result = {
            // Installation, disconnect, and rollback hold the same lock so the
            // owned source obligation stays aligned with its generation.
            let mut subscriptions = server.subscriptions.lock();
            match server
                .publish
                .install_connection(reservation.topic_id, reservation.ticket.generation())
            {
                Ok(installed) => {
                    subscriptions.insert(reservation.topic_id, subscription);
                    reservation.runtime().record_shutdown_event(
                        crate::shutdown_trace::ShutdownEvent::AddSubscription,
                    );
                    Ok((installed.latest, installed.observed_sequence))
                }
                Err(_) => Err(subscription),
            }
        };
        let (value, observed_sequence) = match install_result {
            Ok(installed) => installed,
            Err(subscription) => {
                let cleanup_result = subscription.disconnect_and_wait();
                if let Err(error) = &cleanup_result {
                    reservation
                        .runtime()
                        .record_cleanup_result(Err(error.clone()));
                }
                let rollback_result = self.rollback_with_result();
                if let Some(error) = cleanup_result.err().or_else(|| rollback_result.err()) {
                    return Err(error);
                }
                return Err(XllError::Closing);
            }
        };
        Ok(self.transition(InstalledConnection {
            value,
            observed_sequence,
        }))
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ConnectionTransaction<H, E, C, InstalledConnection>
{
    pub(crate) fn value(&self) -> &StoredRtdValue {
        &self.state.value
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        let reservation = self.reservation();
        if let Err(error) = reservation.runtime().commit_connection(
            reservation.operation.server(),
            reservation.topic_id,
            &reservation.ticket,
            reservation.id,
            self.state.observed_sequence,
        ) {
            self.rollback();
            return Err(error);
        }
        self.reservation.take();
        Ok(())
    }

    pub(crate) fn rollback(self) {
        let _ = self.rollback_with_result();
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>, S: ConnectionStage>
    Drop for ConnectionTransaction<H, E, C, S>
{
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            let _ = reservation.rollback(S::HAS_PUBLICATION);
        }
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop
    for SubscriptionRuntime<H, E, C>
{
    fn drop(&mut self) {
        // Ownership includes joining every producer that can retain a raw sink.
        // Dropping only the operation gates would leave idle producers able to
        // access reclaimed publish cores after the server arena is destroyed.
        let _ = self.close();
    }
}

#[cfg(all(test, feature = "rtd"))]
mod typestate_tests {
    use super::super::tests::SourceFixture;
    use super::super::topic::RtdTopic;
    use super::*;
    use crate::excel_rtd::RtdSubscriptionHost;

    #[test]
    fn miri_connection_typestate_early_stage_drop_releases_each_obligation() {
        for reserve_publication in [false, true] {
            let fixture = SourceFixture::new();
            let (source, _, _) = fixture.add::<i32>(None);
            let runtime = SubscriptionRuntime::<RtdSubscriptionHost, _>::with_sources_for_internal(
                fixture.finish(),
            );
            let server = runtime.register_test_server(1);
            let prepared = runtime
                .prepare(
                    &source,
                    RtdTopic::single("early-stage-drop").unwrap().borrowed(),
                )
                .unwrap();
            let id = prepared.id();
            let generation = ConnectionGeneration::new(1).unwrap();
            let ticket = runtime
                .catalog
                .lock()
                .with_entry(id, |entry| {
                    entry.begin_connection(server.generation(), generation)
                })
                .unwrap()
                .unwrap();
            let transaction = ConnectionTransaction {
                reservation: Some(ConnectionReservation {
                    runtime: NonNull::from(&runtime),
                    // SAFETY: runtime is local and retains this stage until drop.
                    operation: unsafe { server.test_server().enter_owned_operation().unwrap() },
                    topic_id: TopicId(1),
                    id,
                    ticket,
                }),
                state: CatalogReserved,
            };
            if reserve_publication {
                let transaction = transaction.reserve_publication().unwrap();
                assert_eq!(runtime.services.active_quota.used(), 1);
                drop(transaction);
            } else {
                drop(transaction);
            }
            assert_eq!(runtime.services.active_quota.used(), 0);
            assert!(server.test_server().subscriptions.lock().is_empty());
            assert!(!runtime.catalog.lock().entries[&id].is_connected());
            prepared.rollback();
            assert!(runtime.catalog.lock().entries.is_empty());
            runtime.catalog.lock().assert_identity_invariants();
        }
    }
}
