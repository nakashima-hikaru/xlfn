//! Whole-runtime termination authority, including source and worker barriers.
//!
//! Only this module can obtain the primitive coordinator owner for a runtime.
//! The outer orchestration consumes stages; it cannot publish success or pair
//! a legitimate termination owner with a different runtime.

use super::*;
use crate::subscription::termination::{
    TerminationClaim, TerminationCompleted, TerminationOwner, TerminationWaiter,
};
use xlfn_kernel::operation_gate::TerminationWaitGuard;

#[derive(Default)]
pub(in crate::subscription) struct RuntimeTerminationControl {
    coordinator: TerminationCoordinator,
}

impl RuntimeTerminationControl {
    #[cfg(test)]
    pub(in crate::subscription) fn phase(
        &self,
    ) -> crate::subscription::termination::TerminationPhase {
        self.coordinator.phase()
    }
}

pub(super) struct Claimed;
pub(super) struct StopRequested<'runtime> {
    wait: TerminationWaitGuard<'runtime>,
}
pub(super) struct SinkUsersDrained;
pub(super) struct WorkersJoined;

#[must_use = "consume each shutdown stage; abandonment records failure, never completion"]
pub(super) struct RuntimeTermination<
    'runtime,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
    S,
> {
    runtime: &'runtime SubscriptionRuntime<H, E, C>,
    owner: TerminationOwner<'runtime>,
    state: S,
}

pub(super) enum RuntimeTerminationAdmission<
    'runtime,
    H: SubscriptionHost,
    E: RtdSubscription,
    C: crate::RtdSourceCatalog<E>,
> {
    Owner(RuntimeTermination<'runtime, H, E, C, Claimed>),
    Waiter(TerminationWaiter<'runtime>),
    Complete(TerminationCompleted),
}

impl<'runtime, H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>, S>
    RuntimeTermination<'runtime, H, E, C, S>
{
    fn transition<N>(self, state: N) -> RuntimeTermination<'runtime, H, E, C, N> {
        RuntimeTermination {
            runtime: self.runtime,
            owner: self.owner,
            state,
        }
    }

    fn fail(self, error: XllError) -> XllError {
        let first = self.runtime.services.record_cleanup_failure(error);
        self.owner.fail(first)
    }
}

impl<'runtime, H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    RuntimeTermination<'runtime, H, E, C, Claimed>
{
    pub(super) fn begin(
        runtime: &'runtime SubscriptionRuntime<H, E, C>,
    ) -> RuntimeTerminationAdmission<'runtime, H, E, C> {
        match runtime.termination_coordinator.coordinator.begin() {
            TerminationClaim::Owner(owner) => RuntimeTerminationAdmission::Owner(Self {
                runtime,
                owner,
                state: Claimed,
            }),
            TerminationClaim::Waiter(waiter) => RuntimeTerminationAdmission::Waiter(waiter),
            TerminationClaim::Complete(completed) => {
                RuntimeTerminationAdmission::Complete(completed)
            }
        }
    }

    pub(super) fn request_stop(
        self,
    ) -> RuntimeTermination<'runtime, H, E, C, StopRequested<'runtime>> {
        let wait = self.runtime.services.runtime_gate.close_and_wait_begin();
        // A deferred disconnect can hold admission while waiting for shared
        // work to stop. Request every source before waiting on that admission.
        // SourceArena contains each panic and continues through all hooks.
        self.runtime
            .record_cleanup_result(self.runtime.sources.request_stop());
        self.transition(StopRequested { wait })
    }
}

impl<'runtime, H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    RuntimeTermination<'runtime, H, E, C, StopRequested<'runtime>>
{
    pub(super) fn drain_sink_users(
        self,
    ) -> RuntimeTermination<'runtime, H, E, C, SinkUsersDrained> {
        self.state.wait.wait();
        let runtime = self.runtime;
        let server_pointers = {
            let servers = runtime.servers.lock();
            servers
                .values()
                .map(|server| NonNull::from(server.as_ref()))
                .collect::<Vec<_>>()
        };
        let admissions = server_pointers
            .iter()
            .map(|pointer| {
                // SAFETY: the retained runtime keeps all server tombstones
                // alive through this entire termination transaction.
                unsafe { pointer.as_ref() }.begin_termination(runtime)
            })
            .collect::<Vec<_>>();
        // Seal all servers, then notify every cancellation owner, before
        // waiting for any server's sink-user barrier.
        let canceled = admissions
            .into_iter()
            .map(TerminationAdmission::request_cancel)
            .collect::<Vec<_>>();
        let mut first_error = None;
        for admission in canceled {
            if let Err(error) = admission.finish()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            runtime.record_cleanup_result(Err(error));
        }
        RuntimeTermination {
            runtime,
            owner: self.owner,
            state: SinkUsersDrained,
        }
    }
}

impl<'runtime, H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    RuntimeTermination<'runtime, H, E, C, SinkUsersDrained>
{
    pub(super) fn join_workers(
        self,
    ) -> XllResult<RuntimeTermination<'runtime, H, E, C, WorkersJoined>> {
        // Outer failure means no join hook executed. An inner error follows
        // the source's unsafe contract that its barrier still completed.
        match self.runtime.join_sources_with_dispatch() {
            Ok(result) => self.runtime.record_cleanup_result(result),
            Err(error) => return Err(self.fail(error)),
        }
        // Keep the STA dispatchable through the disconnect worker's final
        // COM teardown; preparation failure retains that worker's owner.
        if let Err(error) = self.runtime.disconnects.stop() {
            return Err(self.fail(error));
        }
        #[cfg(feature = "rtd")]
        self.runtime.services.publishers.stop();
        Ok(self.transition(WorkersJoined))
    }
}

impl<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>
    RuntimeTermination<'_, H, E, C, WorkersJoined>
{
    pub(super) fn finish(self) -> XllResult<()> {
        {
            let mut catalog = self.runtime.catalog.lock();
            catalog.identities.clear();
            catalog.pending_topic_bytes = 0;
            catalog.pending_count = 0;
            catalog.entries.clear();
        }
        self.owner.complete(self.runtime.cleanup_result())
    }
}

#[cfg(all(test, feature = "rtd"))]
mod tests;
