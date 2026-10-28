#![allow(
    unsafe_code,
    reason = "server handles are audited non-owning generational capabilities into a runtime-owned arena"
)]

use super::catalog::SubscriptionCatalog;
use super::data_plane::{
    OwnedPublishOperation, PublishCore, PublishTerminationStart, RtdRefreshBatch,
    ScopedPublishOperation,
};
use super::disconnect::OwnedSubscription;
use super::host::SubscriptionHost;
use super::runtime::{SubscriptionConnection, SubscriptionRuntime};
use super::source::{RtdSubscription, SourceSubscription};
use super::termination::{
    TerminationClaim, TerminationCompleted, TerminationCoordinator, TerminationOwner,
    TerminationWaiter,
};
use super::topic::{SubscriptionId, TopicId};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::panic_boundary::catch_no_unwind;
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;

/// Generational, non-owning access to one server retained by a subscription
/// runtime arena.
pub(crate) struct SubscriptionServerHandle<
    H: SubscriptionHost,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    runtime: NonNull<SubscriptionRuntime<H, E, C>>,
    server: NonNull<SubscriptionServer<H, E, C>>,
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Copy
    for SubscriptionServerHandle<H, E, C>
{
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Clone
    for SubscriptionServerHandle<H, E, C>
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    SubscriptionServerHandle<H, E, C>
{
    /// Constructs a generational handle to a registered subscription server.
    ///
    /// # Safety
    ///
    /// `server` must point to a registered allocation owned by `runtime` and
    /// retained without replacement. Keep both allocations stable through every
    /// method call and returned guard/batch. Inert copies may outlive reclamation
    /// only when they are never used again; their Drop does not dereference them.
    pub(super) unsafe fn new(
        runtime: &SubscriptionRuntime<H, E, C>,
        server: NonNull<SubscriptionServer<H, E, C>>,
    ) -> Self {
        Self {
            runtime: NonNull::from(runtime),
            server,
        }
    }

    #[inline]
    pub(super) fn belongs_to(&self, runtime: &SubscriptionRuntime<H, E, C>) -> bool {
        self.runtime == NonNull::from(runtime)
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn generation(&self) -> ServerGeneration {
        self.server().expect("registered server").generation
    }

    #[inline]
    fn runtime(&self) -> &SubscriptionRuntime<H, E, C> {
        // SAFETY: the COM/server lifecycle contract drains every handle use
        // before the runtime service is sealed and reclaimed.
        unsafe { self.runtime.as_ref() }
    }

    #[inline]
    pub(crate) fn server(&self) -> XllResult<&SubscriptionServer<H, E, C>> {
        // SAFETY: registration retains this stable allocation until the runtime
        // and every handle use have been drained. No entry is removed or replaced.
        Ok(unsafe { self.server.as_ref() })
    }

    pub(crate) fn attach_update_notifier(
        &self,
        notifier: H::Notifier,
    ) -> XllResult<Option<H::Notifier>> {
        self.server()?.publish.attach_update_notifier(notifier)
    }

    pub(crate) fn detach_update_notifier(&self) -> Option<H::Notifier> {
        self.server()
            .ok()
            .and_then(|server| server.publish.detach_update_notifier())
    }

    pub(crate) fn pulse_notification(&self) -> XllResult<()> {
        self.server()?.publish.pulse_notification()
    }

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch<'_, H>> {
        self.server()?.publish.begin_refresh()
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        self.server()
            .expect("test server remains registered")
            .publish
            .pending_update_count()
    }

    #[cfg(test)]
    pub(crate) fn test_server(&self) -> &SubscriptionServer<H, E, C> {
        self.server().expect("test server remains registered")
    }

    pub(crate) fn claim(&self, id: SubscriptionId) -> XllResult<()> {
        let runtime = self.runtime();
        let _runtime_operation = runtime.enter_external_operation()?;
        let server = self.server()?;
        let _server_operation = server.enter_operation()?;
        server.ensure_open()?;
        runtime.claim_server(server.generation, id)
    }

    #[cfg(all(windows, any(feature = "rtd", feature = "handles")))]
    pub(crate) fn resolve_transport_key(
        &self,
        key: super::topic::SubscriptionKey,
    ) -> XllResult<SubscriptionId> {
        self.runtime().resolve_transport_key(key)
    }

    pub(crate) fn connect_transaction(
        &self,
        topic_id: TopicId,
        id: SubscriptionId,
    ) -> XllResult<SubscriptionConnection<H, E, C>> {
        self.runtime().connect_transaction(self, topic_id, id)
    }

    #[cfg_attr(
        windows,
        allow(
            dead_code,
            reason = "synchronous backend disconnection retains its sink-user barrier contract; COM uses the deferred entry point"
        )
    )]
    pub(crate) fn disconnect(&self, topic_id: TopicId) -> XllResult<()> {
        self.runtime().disconnect(self, topic_id)
    }

    pub(crate) fn disconnect_deferred(
        &self,
        topic_id: TopicId,
        completion: impl Into<H::DisconnectCompletion>,
    ) -> XllResult<()> {
        self.runtime()
            .disconnect_deferred(self, topic_id, completion.into())
    }

    pub(crate) fn terminate(&self) -> XllResult<()> {
        self.runtime().terminate_server(self.server()?.generation)
    }
}

// SAFETY: the runtime and server are thread-safe; the temporal validity proof
// is the same server/module quiescence contract used by every handle method.
unsafe impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Send
    for SubscriptionServerHandle<H, E, C>
{
}
// SAFETY: SubscriptionServerHandle is a Copy non-owning handle with no interior mutability.
unsafe impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Sync
    for SubscriptionServerHandle<H, E, C>
{
}

pub(crate) struct SubscriptionServer<
    H: SubscriptionHost,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    pub(crate) generation: ServerGeneration,
    pub(crate) publish: xlfn_kernel::published_owner::PublishedOwner<PublishCore<H>>,
    pub(super) subscriptions: Mutex<FxHashMap<TopicId, OwnedSubscription<H, E, C>>>,
    pub(super) termination_coordinator: TerminationCoordinator,
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> std::fmt::Debug
    for SubscriptionServer<H, E, C>
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubscriptionServer")
            .field("generation", &self.generation)
            .field("publish", &self.publish)
            .finish_non_exhaustive()
    }
}

pub(crate) struct OwnedServerOperation<
    H: SubscriptionHost,
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    server: NonNull<SubscriptionServer<H, E, C>>,
    pub(crate) _publish_operation: OwnedPublishOperation<H>,
}

// SAFETY: the nested publish operation guards ensure liveness of the server
// across thread boundaries for the duration of the owned operation.
unsafe impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Send
    for OwnedServerOperation<H, E, C>
{
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    OwnedServerOperation<H, E, C>
{
    #[inline]
    pub(crate) fn server(&self) -> &SubscriptionServer<H, E, C> {
        // SAFETY: the nested runtime/server operation guards prevent teardown
        // while this non-owning pointer is used.
        unsafe { self.server.as_ref() }
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    SubscriptionServer<H, E, C>
{
    #[inline]
    pub(crate) fn ensure_open(&self) -> XllResult<()> {
        self.publish.ensure_open()
    }

    #[inline]
    pub(crate) fn enter_operation(&self) -> XllResult<ScopedPublishOperation<'_, H>> {
        self.publish.enter_operation()
    }

    #[inline]
    /// Enters an operation whose guard is allowed to outlive this borrow.
    ///
    /// # Safety
    ///
    /// The caller must keep this server and its owning subscription runtime
    /// allocated until the returned operation is dropped. In particular, the
    /// runtime and server must seal and drain their operation gates before
    /// reclaiming either object.
    pub(crate) unsafe fn enter_owned_operation(&self) -> XllResult<OwnedServerOperation<H, E, C>> {
        // SAFETY: upheld by this function's caller; the publish core is
        // runtime-owned and its operation guard keeps both gates admitted.
        let publish_operation = unsafe { self.publish.enter_owned_operation()? };
        Ok(OwnedServerOperation {
            server: NonNull::from(self),
            _publish_operation: publish_operation,
        })
    }

    pub(super) fn begin_termination<'a>(
        &'a self,
        runtime: &'a SubscriptionRuntime<H, E, C>,
    ) -> TerminationAdmission<'a, H, E, C> {
        match self.termination_coordinator.begin() {
            TerminationClaim::Owner(owner) => {
                // Install the unique failure guard before touching publish state.
                // Its resources are cleaned before failed completion is published.
                let mut resources = ServerTerminationResources {
                    runtime,
                    server: self,
                    wait: None,
                    notifier: None,
                    initial_subscriptions: FxHashMap::default(),
                    owner: Some(owner),
                };
                let mut termination = self.publish.begin_termination();
                resources.notifier = termination.take_notifier();
                resources.wait = Some(termination);
                resources.initial_subscriptions = std::mem::take(&mut *self.subscriptions.lock());
                TerminationAdmission::Owner(ServerTermination {
                    resources: Some(resources),
                    state: CancelPending,
                })
            }
            TerminationClaim::Waiter(waiter) => TerminationAdmission::Waiter(waiter),
            TerminationClaim::Complete(completed) => TerminationAdmission::Complete(completed),
        }
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop
    for SubscriptionServer<H, E, C>
{
    fn drop(&mut self) {
        self.publish.close_on_server_drop();
    }
}

pub(super) enum TerminationAdmission<
    'a,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
> {
    Owner(ServerTermination<'a, H, E, C, CancelPending>),
    Waiter(TerminationWaiter<'a>),
    Complete(TerminationCompleted),
}

impl<'a, H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    TerminationAdmission<'a, H, E, C>
{
    /// Notify every owner before the caller waits for any one server.
    pub(super) fn request_cancel(self) -> CancelRequestedAdmission<'a, H, E, C> {
        match self {
            Self::Owner(owner) => CancelRequestedAdmission::Owner(owner.request_cancel()),
            Self::Waiter(waiter) => CancelRequestedAdmission::Waiter(waiter),
            Self::Complete(completed) => CancelRequestedAdmission::Complete(completed),
        }
    }
}

pub(super) enum CancelRequestedAdmission<
    'a,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
> {
    Owner(ServerTermination<'a, H, E, C, CancelRequested>),
    Waiter(TerminationWaiter<'a>),
    Complete(TerminationCompleted),
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    CancelRequestedAdmission<'_, H, E, C>
{
    pub(super) fn finish(self) -> XllResult<()> {
        match self {
            Self::Owner(owner) => owner.finish(),
            Self::Waiter(waiter) => waiter.wait(),
            Self::Complete(completed) => completed.result(),
        }
    }
}

#[allow(
    clippy::drop_non_drop,
    reason = "RtdNotifier contains drop types on Windows/test configurations but may be uninhabited on non-Windows production"
)]
pub(crate) fn drop_notifier_no_unwind<N>(notifier: Option<N>) -> XllResult<()> {
    catch_no_unwind(AssertUnwindSafe(|| drop(notifier))).map_err(|_| XllError::Panic)
}

thread_local! {
    pub(crate) static PANIC_AFTER_TERMINATION_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) trait TerminationStage {
    const CANCEL_PENDING: bool;
}

pub(super) struct CancelPending;
pub(super) struct CancelRequested {
    error: Option<XllError>,
}

impl TerminationStage for CancelPending {
    const CANCEL_PENDING: bool = true;
}

impl TerminationStage for CancelRequested {
    const CANCEL_PENDING: bool = false;
}

/// Resources precede the coordinator owner so abandonment cannot publish a
/// completed attempt before its captured source obligations have been released.
struct ServerTerminationResources<
    'a,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
> {
    runtime: &'a SubscriptionRuntime<H, E, C>,
    server: &'a SubscriptionServer<H, E, C>,
    wait: Option<PublishTerminationStart<'a, H>>,
    notifier: Option<H::Notifier>,
    initial_subscriptions: FxHashMap<TopicId, OwnedSubscription<H, E, C>>,
    owner: Option<TerminationOwner<'a>>,
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ServerTerminationResources<'_, H, E, C>
{
    fn request_cancel(&mut self) -> XllResult<()> {
        let mut first_error = None;
        for subscription in self.initial_subscriptions.values_mut() {
            if catch_no_unwind(AssertUnwindSafe(|| subscription.request_cancel())).is_err()
                && first_error.is_none()
            {
                first_error = Some(XllError::Panic);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Drop
    for ServerTerminationResources<'_, H, E, C>
{
    fn drop(&mut self) {
        // Even an unexpected unwind must contain notifier/source destructors.
        // The affine owner is dropped last and publishes Failed, never drained.
        self.runtime
            .record_cleanup_result(drop_notifier_no_unwind(self.notifier.take()));
        let subscriptions = std::mem::take(&mut self.initial_subscriptions);
        for _ in 0..subscriptions.len() {
            self.runtime
                .record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveSubscription);
        }
        self.runtime
            .record_cleanup_result(disconnect_all_no_unwind(subscriptions.into_values()));
        if self.owner.is_some() {
            self.runtime.record_cleanup_result(Err(XllError::Panic));
        }
    }
}

#[must_use = "request cancellation and finish termination; abandonment publishes failure"]
pub(super) struct ServerTermination<
    'a,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
    S: TerminationStage,
> {
    resources: Option<ServerTerminationResources<'a, H, E, C>>,
    state: S,
}

impl<'a, H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ServerTermination<'a, H, E, C, CancelPending>
{
    pub(super) fn request_cancel(mut self) -> ServerTermination<'a, H, E, C, CancelRequested> {
        let result = self
            .resources
            .as_mut()
            .expect("live termination")
            .request_cancel();
        ServerTermination {
            resources: self.resources.take(),
            state: CancelRequested {
                error: result.err(),
            },
        }
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>, S: TerminationStage>
    Drop for ServerTermination<'_, H, E, C, S>
{
    fn drop(&mut self) {
        if let Some(mut resources) = self.resources.take() {
            if S::CANCEL_PENDING {
                resources
                    .runtime
                    .record_cleanup_result(resources.request_cancel());
            }
            drop(resources);
        }
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    ServerTermination<'_, H, E, C, CancelRequested>
{
    pub(super) fn finish(mut self) -> XllResult<()> {
        let mut resources = self.resources.take().expect("live termination");
        #[cfg(test)]
        if PANIC_AFTER_TERMINATION_GUARD.replace(false) {
            panic!("injected termination owner panic");
        }

        let mut first_error = self.state.error.take();
        if let Err(error) = drop_notifier_no_unwind(resources.notifier.take())
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        if catch_no_unwind(AssertUnwindSafe(|| {
            resources
                .wait
                .take()
                .expect("termination owns its admission wait")
                .wait()
        }))
        .is_err()
            && first_error.is_none()
        {
            first_error = Some(XllError::Panic);
        }

        let (late_notifier, active_entries) =
            resources.server.publish.finish_termination().into_parts();
        if let Err(error) = drop_notifier_no_unwind(late_notifier)
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        {
            let mut catalog = resources.runtime.catalog.lock();
            for topic in &active_entries {
                cleanup_catalog_binding_and_pending(
                    &mut catalog,
                    topic.id,
                    resources.server.generation,
                    topic.generation,
                );
            }

            let pending_ids = catalog
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !entry.is_active()
                        && entry.server_generation() == Some(resources.server.generation)
                })
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            for id in pending_ids {
                let Some(should_remove) = catalog.with_entry(id, |entry| {
                    entry.reset_for_server_termination(resources.server.generation)
                        && entry.can_remove()
                }) else {
                    continue;
                };
                if should_remove {
                    catalog.remove_entry(id);
                }
            }
        }

        let subscriptions = std::mem::take(&mut resources.initial_subscriptions);
        let late_subscriptions = std::mem::take(&mut *resources.server.subscriptions.lock());
        for _ in 0..subscriptions.len() + late_subscriptions.len() {
            resources
                .runtime
                .record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveSubscription);
        }
        if let Err(error) = disconnect_all_no_unwind(
            subscriptions
                .into_values()
                .chain(late_subscriptions.into_values()),
        ) && first_error.is_none()
        {
            first_error = Some(error);
        }

        let result = first_error.map_or(Ok(()), Err);
        resources.runtime.record_cleanup_result(result.clone());
        let owner = resources.owner.take().expect("live termination owner");
        drop(resources);
        owner.complete(result)
    }
}

pub(crate) fn disconnect_one_no_unwind<E: RtdSubscription>(
    subscription: SourceSubscription<E>,
) -> XllResult<()> {
    match catch_no_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait())) {
        Ok(result) => result,
        Err(_) => Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::PANIC_DISCONNECT,
        }),
    }
}

fn disconnect_all_no_unwind<
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
>(
    subscriptions: impl IntoIterator<Item = OwnedSubscription<H, E, C>>,
) -> XllResult<()> {
    let mut first_error = None;
    for subscription in subscriptions {
        if let Err(error) = subscription.disconnect_and_wait()
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(crate) fn cleanup_catalog_binding_and_pending(
    catalog: &mut SubscriptionCatalog,
    id: SubscriptionId,
    server_generation: ServerGeneration,
    connection_generation: ConnectionGeneration,
) {
    let Some((_, should_remove)) = catalog.with_entry(id, |entry| {
        if entry.connection_generation() != Some(connection_generation)
            || entry.server_generation() != Some(server_generation)
        {
            return (false, false);
        }
        entry.cleanup_connection(server_generation, connection_generation)
    }) else {
        return;
    };
    if should_remove {
        catalog.remove_entry(id);
    }
}

#[cfg(all(test, feature = "rtd"))]
#[path = "server/termination_tests.rs"]
mod termination_tests;
