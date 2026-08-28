use super::catalog::{SubscriptionCatalog, SubscriptionEntry};
use super::data_plane::{
    OwnedPublishOperation, PublishCore, PublishTerminationStart, RtdRefreshBatch,
    ScopedPublishOperation,
};
use super::host::SubscriptionHost;
use super::runtime::{SubscriptionConnection, SubscriptionRuntime};
use super::source::{ErasedRtdSource, RtdSubscription};
use super::topic::{SubscriptionId, TopicId};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use rustc_hash::FxHashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Weak};

#[derive(Clone)]
pub(crate) struct SubscriptionServerHandle<H: SubscriptionHost> {
    pub(crate) inner: Arc<SubscriptionServer<H>>,
}

impl<H: SubscriptionHost> SubscriptionServerHandle<H> {
    pub(crate) fn attach_update_notifier(
        &self,
        notifier: H::Notifier,
    ) -> XllResult<Option<H::Notifier>> {
        self.inner.publish.attach_update_notifier(notifier)
    }

    pub(crate) fn detach_update_notifier(&self) -> Option<H::Notifier> {
        self.inner.publish.detach_update_notifier()
    }

    pub(crate) fn pulse_notification(&self) -> XllResult<()> {
        self.inner.publish.pulse_notification()
    }

    pub(crate) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch<'_, H>> {
        self.inner.publish.begin_refresh()
    }

    #[cfg(test)]
    pub(crate) fn pending_update_count(&self) -> usize {
        self.inner.publish.pending_update_count()
    }

    pub(crate) fn claim(&self, id: SubscriptionId) -> XllResult<()> {
        let _operation = self.inner.enter_operation()?;
        self.inner.ensure_open()?;
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.claim_server(self.inner.generation, id)
    }

    pub(crate) fn connect_transaction(
        &self,
        topic_id: TopicId,
        id: SubscriptionId,
    ) -> XllResult<SubscriptionConnection<H>> {
        let parent = self.inner.parent.upgrade().ok_or(XllError::Closing)?;
        parent.connect_transaction(self, topic_id, id)
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

pub(crate) struct SubscriptionServer<H: SubscriptionHost> {
    pub(crate) generation: ServerGeneration,
    pub(crate) publish: triomphe::Arc<PublishCore<H>>,
    pub(crate) subscriptions: Mutex<FxHashMap<TopicId, Box<dyn RtdSubscription>>>,
    pub(crate) parent: Weak<SubscriptionRuntime<H>>,
    pub(crate) termination_coordinator: TerminationCoordinator,
}

impl<H: SubscriptionHost> std::fmt::Debug for SubscriptionServer<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionServer")
            .field("generation", &self.generation)
            .field("publish", &self.publish)
            .finish_non_exhaustive()
    }
}

pub(crate) struct OwnedServerOperation<H: SubscriptionHost> {
    pub(crate) server: Arc<SubscriptionServer<H>>,
    pub(crate) _publish_operation: OwnedPublishOperation<H>,
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
    pub(crate) fn enter_owned_operation(self: &Arc<Self>) -> XllResult<OwnedServerOperation<H>> {
        let publish_operation =
            PublishCore::enter_owned_operation(triomphe::Arc::clone(&self.publish))?;
        Ok(OwnedServerOperation {
            server: Arc::clone(self),
            _publish_operation: publish_operation,
        })
    }

    pub(crate) fn remove_from_registry(&self) {
        if let Some(parent) = self.parent.upgrade() {
            let mut servers = parent.servers.lock();
            servers.remove(&self.generation);
        }
    }

    pub(crate) fn begin_termination<'a>(self: &'a Arc<Self>) -> TerminationAdmission<'a, H> {
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

                let initial_subscriptions: Vec<_> = self
                    .subscriptions
                    .lock()
                    .drain()
                    .map(|(_, sub)| sub)
                    .collect();

                TerminationAdmission::Owner(ServerTermination {
                    server: Arc::clone(self),
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
    pub(crate) server: Arc<SubscriptionServer<H>>,
    pub(crate) wait: PublishTerminationStart<'a, H>,
    pub(crate) notifier: Option<H::Notifier>,
    pub(crate) initial_subscriptions: Vec<Box<dyn RtdSubscription>>,
}

impl<'a, H: SubscriptionHost> ServerTermination<'a, H> {
    pub(crate) fn request_cancel(&self) -> XllResult<()> {
        let mut first_error = None;
        for sub in &self.initial_subscriptions {
            if catch_unwind(AssertUnwindSafe(|| sub.cancellation().request_cancel())).is_err()
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

        if let Err(err) = drop_notifier_no_unwind(self.notifier.take())
            && first_error.is_none()
        {
            first_error = Some(err);
        }

        let wait = self.wait;
        let wait_res = catch_unwind(AssertUnwindSafe(|| wait.wait()));
        if wait_res.is_err() && first_error.is_none() {
            first_error = Some(XllError::Panic);
        }

        let (late_notifier, active_entries) = self.server.publish.finish_termination().into_parts();

        if let Some(parent) = self.server.parent.upgrade() {
            for _ in 0..self.initial_subscriptions.len() {
                parent.record_shutdown_event(
                    crate::shutdown_trace::ShutdownEvent::RemoveSubscription,
                );
            }
        }

        if let Err(err) = drop_notifier_no_unwind(late_notifier)
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
                    topic.id,
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
            let unactive_pending_ids: Vec<_> = catalog
                .entries
                .iter()
                .filter(|(_, entry)| {
                    !entry.is_active() && entry.server_generation() == Some(self.server.generation)
                })
                .map(|(id, _)| *id)
                .collect();

            let mut extra_sources = Vec::new();
            for id in unactive_pending_ids {
                let Some(should_remove) = catalog.with_entry(id, |entry| {
                    entry.reset_for_server_termination(self.server.generation) && entry.can_remove()
                }) else {
                    continue;
                };

                if should_remove
                    && let Some(removed) = catalog.remove_entry(id)
                    && let Some(source) = removed.into_source()
                {
                    extra_sources.push(source);
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

        let all_subscriptions: Vec<Box<dyn RtdSubscription>> = self
            .initial_subscriptions
            .drain(..)
            .chain(self.server.subscriptions.lock().drain().map(|(_, s)| s))
            .collect();

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
    id: SubscriptionId,
    server_generation: ServerGeneration,
    conn_generation: ConnectionGeneration,
) -> Option<Arc<dyn ErasedRtdSource>> {
    let (_, should_remove) = catalog.with_entry(id, |entry| {
        if entry.connection_generation() != Some(conn_generation)
            || entry.server_generation() != Some(server_generation)
        {
            return (false, false);
        }

        let (matched, should_remove) = entry.cleanup_connection(server_generation, conn_generation);
        (matched, should_remove)
    })?;

    if should_remove {
        return catalog
            .remove_entry(id)
            .and_then(SubscriptionEntry::into_source);
    }

    None
}
