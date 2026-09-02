#![allow(
    unsafe_code,
    reason = "server handles are audited non-owning generational capabilities into a runtime-owned arena"
)]

use super::catalog::SubscriptionCatalog;
use super::data_plane::{
    OwnedPublishOperation, PublishCore, PublishTerminationStart, RtdRefreshBatch,
    ScopedPublishOperation,
};
use super::host::SubscriptionHost;
use super::runtime::{SubscriptionConnection, SubscriptionRuntime};
use super::source::RtdSubscription;
use super::topic::{SubscriptionId, TopicId};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use rustc_hash::FxHashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

/// Generational, non-owning access to one server retained by a subscription
/// runtime arena.
pub(crate) struct SubscriptionServerHandle<H: SubscriptionHost> {
    runtime: NonNull<SubscriptionRuntime<H>>,
    generation: ServerGeneration,
}

impl<H: SubscriptionHost> Copy for SubscriptionServerHandle<H> {}

impl<H: SubscriptionHost> Clone for SubscriptionServerHandle<H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: SubscriptionHost> SubscriptionServerHandle<H> {
    pub(super) fn new(runtime: &SubscriptionRuntime<H>, generation: ServerGeneration) -> Self {
        Self {
            runtime: NonNull::from(runtime),
            generation,
        }
    }

    #[inline]
    pub(crate) const fn generation(&self) -> ServerGeneration {
        self.generation
    }

    #[inline]
    fn runtime(&self) -> &SubscriptionRuntime<H> {
        // SAFETY: the COM/server lifecycle contract drains every handle use
        // before the runtime service is sealed and reclaimed.
        unsafe { self.runtime.as_ref() }
    }

    #[inline]
    pub(crate) fn server(&self) -> XllResult<&SubscriptionServer<H>> {
        self.runtime()
            .resolve_server(self.generation)
            .ok_or(XllError::Closing)
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
    pub(crate) fn test_server(&self) -> &SubscriptionServer<H> {
        self.server().expect("test server remains registered")
    }

    pub(crate) fn claim(&self, id: SubscriptionId) -> XllResult<()> {
        let runtime = self.runtime();
        let _runtime_operation = runtime.enter_external_operation()?;
        let server = self.server()?;
        let _server_operation = server.enter_operation()?;
        server.ensure_open()?;
        runtime.claim_server(self.generation, id)
    }

    pub(crate) fn connect_transaction(
        &self,
        topic_id: TopicId,
        id: SubscriptionId,
    ) -> XllResult<SubscriptionConnection<H>> {
        self.runtime().connect_transaction(self, topic_id, id)
    }

    pub(crate) fn disconnect(&self, topic_id: TopicId) -> XllResult<()> {
        self.runtime().disconnect(self, topic_id)
    }

    pub(crate) fn terminate(&self) -> XllResult<()> {
        self.runtime().terminate_server(self.generation)
    }
}

// SAFETY: the runtime and server are thread-safe; the temporal validity proof
// is the same server/module quiescence contract used by every handle method.
unsafe impl<H: SubscriptionHost> Send for SubscriptionServerHandle<H> {}
// SAFETY: SubscriptionServerHandle is a Copy non-owning handle with no interior mutability.
unsafe impl<H: SubscriptionHost> Sync for SubscriptionServerHandle<H> {}

pub(crate) struct SubscriptionServer<H: SubscriptionHost> {
    pub(crate) generation: ServerGeneration,
    pub(crate) publish: Box<PublishCore<H>>,
    pub(crate) subscriptions: Mutex<FxHashMap<TopicId, Box<dyn RtdSubscription>>>,
    pub(crate) termination_coordinator: TerminationCoordinator,
}

impl<H: SubscriptionHost> std::fmt::Debug for SubscriptionServer<H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubscriptionServer")
            .field("generation", &self.generation)
            .field("publish", &self.publish)
            .finish_non_exhaustive()
    }
}

pub(crate) struct OwnedServerOperation<H: SubscriptionHost> {
    server: NonNull<SubscriptionServer<H>>,
    pub(crate) _publish_operation: OwnedPublishOperation<H>,
}

// SAFETY: the nested publish operation guards ensure liveness of the server
// across thread boundaries for the duration of the owned operation.
unsafe impl<H: SubscriptionHost> Send for OwnedServerOperation<H> {}

impl<H: SubscriptionHost> OwnedServerOperation<H> {
    #[inline]
    pub(crate) fn server(&self) -> &SubscriptionServer<H> {
        // SAFETY: the nested runtime/server operation guards prevent teardown
        // while this non-owning pointer is used.
        unsafe { self.server.as_ref() }
    }
}

impl<H: SubscriptionHost> SubscriptionServer<H> {
    #[inline]
    pub(crate) fn ensure_open(&self) -> XllResult<()> {
        self.publish.ensure_open()
    }

    #[inline]
    pub(crate) fn enter_operation(&self) -> XllResult<ScopedPublishOperation<'_, H>> {
        self.publish.enter_operation()
    }

    #[inline]
    pub(crate) fn enter_owned_operation(&self) -> XllResult<OwnedServerOperation<H>> {
        let publish_operation = self.publish.enter_owned_operation()?;
        Ok(OwnedServerOperation {
            server: NonNull::from(self),
            _publish_operation: publish_operation,
        })
    }

    pub(crate) fn begin_termination<'a>(
        &'a self,
        runtime: &'a SubscriptionRuntime<H>,
    ) -> TerminationAdmission<'a, H> {
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
                let mut termination = self.publish.begin_termination();
                term_state.phase = ServerTerminationPhase::Terminating;
                let notifier = termination.take_notifier();
                let initial_subscriptions = self
                    .subscriptions
                    .lock()
                    .drain()
                    .map(|(_, subscription)| subscription)
                    .collect();
                TerminationAdmission::Owner(ServerTermination {
                    runtime,
                    server: self,
                    wait: termination,
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
}

impl<H: SubscriptionHost> Drop for SubscriptionServer<H> {
    fn drop(&mut self) {
        self.publish.close_on_server_drop();
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

impl ServerTerminationWaiter<'_> {
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

thread_local! {
    pub(crate) static PANIC_AFTER_TERMINATION_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) struct ServerTermination<'a, H: SubscriptionHost> {
    pub(crate) runtime: &'a SubscriptionRuntime<H>,
    pub(crate) server: &'a SubscriptionServer<H>,
    pub(crate) wait: PublishTerminationStart<'a, H>,
    pub(crate) notifier: Option<H::Notifier>,
    pub(crate) initial_subscriptions: Vec<Box<dyn RtdSubscription>>,
}

impl<H: SubscriptionHost> ServerTermination<'_, H> {
    pub(crate) fn request_cancel(&self) -> XllResult<()> {
        let mut first_error = None;
        for subscription in &self.initial_subscriptions {
            if catch_unwind(AssertUnwindSafe(|| subscription.request_cancel())).is_err()
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
        if let Err(error) = drop_notifier_no_unwind(self.notifier.take())
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        if catch_unwind(AssertUnwindSafe(|| self.wait.wait())).is_err() && first_error.is_none() {
            first_error = Some(XllError::Panic);
        }

        let (late_notifier, active_entries) = self.server.publish.finish_termination().into_parts();
        for _ in 0..self.initial_subscriptions.len() {
            self.runtime
                .record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveSubscription);
        }
        if let Err(error) = drop_notifier_no_unwind(late_notifier)
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        {
            let mut catalog = self.runtime.catalog.lock();
            for topic in &active_entries {
                cleanup_catalog_binding_and_pending(
                    &mut catalog,
                    topic.id,
                    self.server.generation,
                    topic.generation,
                );
            }

            let pending_ids = catalog
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !entry.is_active() && entry.server_generation() == Some(self.server.generation)
                })
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            for id in pending_ids {
                let Some(should_remove) = catalog.with_entry(id, |entry| {
                    entry.reset_for_server_termination(self.server.generation) && entry.can_remove()
                }) else {
                    continue;
                };
                if should_remove {
                    catalog.remove_entry(id);
                }
            }
        }

        let subscriptions = self
            .initial_subscriptions
            .drain(..)
            .chain(
                self.server
                    .subscriptions
                    .lock()
                    .drain()
                    .map(|(_, subscription)| subscription),
            )
            .collect::<Vec<_>>();
        if let Err(error) = disconnect_all_no_unwind(subscriptions)
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        let result = first_error.map_or(Ok(()), Err);
        self.runtime.record_cleanup_result(result.clone());
        guard.complete(result)
    }
}

pub(crate) fn disconnect_one_no_unwind(subscription: Box<dyn RtdSubscription>) -> XllResult<()> {
    match catch_unwind(AssertUnwindSafe(|| subscription.disconnect_and_wait())) {
        Ok(result) => result,
        Err(_) => Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::PANIC_DISCONNECT,
        }),
    }
}

pub(crate) fn disconnect_all_no_unwind(
    subscriptions: impl IntoIterator<Item = Box<dyn RtdSubscription>>,
) -> XllResult<()> {
    let mut first_error = None;
    for subscription in subscriptions {
        if let Err(error) = disconnect_one_no_unwind(subscription)
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
