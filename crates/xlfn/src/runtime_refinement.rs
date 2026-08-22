//! Refinement instrumentation for the runtime protocol.
//!
//! The operational lifecycle transition is owned by `Runtime`; this façade
//! only observes or augments that transition when the refinement model is
//! enabled. Keeping the build configuration here prevents production and
//! refinement builds from carrying separate state-transition implementations.

#[cfg(any(test, feature = "shutdown-refinement"))]
use crate::XllError;
use crate::generation::OpenAttemptId;
use crate::runtime::Runtime;
use crate::{Addin, XllResult};
#[cfg(any(test, feature = "shutdown-refinement"))]
use std::sync::Arc;

pub(crate) struct RuntimeRefinementHooks {
    #[cfg(any(test, feature = "shutdown-refinement"))]
    formal: crate::runtime_components::FormalState,
}

impl RuntimeRefinementHooks {
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            formal: crate::runtime_components::FormalState::new(),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn ghost_handle(&self) -> crate::shutdown_refinement::GhostHandle {
        Arc::clone(
            self.formal
                .ghost
                .get_or_init(|| Arc::new(crate::shutdown_refinement::ShutdownGhost::new())),
        )
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn composition_trace(&self) -> &crate::composition_refinement::CompositionTrace {
        let trace = self
            .formal
            .composition
            .get_or_init(|| Arc::new(crate::composition_refinement::CompositionTrace::new()));
        self.ghost_handle().set_composition(Arc::clone(trace));
        trace.as_ref()
    }

    pub(crate) fn begin_open<A: Addin>(
        &self,
        runtime: &Runtime<A>,
        sampled_epoch: u64,
        attempt: OpenAttemptId,
    ) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            let ghost = runtime.ghost_handle();
            if ghost.active() {
                runtime.return_protocol.returns.set_ghost(ghost);
            }
            runtime.record_composition_begin_open(sampled_epoch, attempt.get());
        }
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        {
            let _ = (runtime, sampled_epoch, attempt);
        }
    }

    pub(crate) fn commit_open<A: Addin>(
        &self,
        runtime: &Runtime<A>,
        attempt: OpenAttemptId,
        operation: impl FnOnce() -> XllResult<()>,
    ) -> XllResult<()> {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            let ghost = runtime.ghost_handle();
            let mut resources = crate::shutdown_refinement::GhostResources::opened(
                runtime.host.registrations_snapshot().len() as u64,
                runtime.host.event_registrations_snapshot().len() as u64,
            );
            #[cfg(feature = "async")]
            {
                resources.async_executor_running = !runtime.executors.async_manager.is_stopped();
            }
            crate::diagnostics::connect_ghost(Arc::clone(&ghost), |snapshot| {
                resources.diagnostics_running = snapshot.running;
                resources.diagnostics_pending = snapshot.pending;
                ghost
                    .begin_generation(attempt.get(), resources.clone())
                    .map_err(|_| XllError::Internal {
                        diagnostic_id: crate::error::DiagnosticId::GHOST_GENERATION,
                    })?;
                operation()
            })?;
            crate::rtd::set_ghost(Arc::clone(&ghost));
            runtime
                .return_protocol
                .returns
                .set_ghost(Arc::clone(&ghost));
            runtime
                .generation_services
                .handles
                .set_ghost(Arc::clone(&ghost));
            runtime
                .generation_services
                .subscriptions
                .set_ghost(Arc::clone(&ghost));
            #[cfg(feature = "async")]
            runtime
                .executors
                .async_manager
                .set_ghost(Arc::clone(&ghost));
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::CommitOpen {
                    attempt: attempt.get(),
                    resources,
                },
            );
            Ok(())
        }
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        {
            let _ = (runtime, attempt);
            operation()
        }
    }

    pub(crate) fn reject_open<A: Addin>(&self, runtime: &Runtime<A>, attempt: OpenAttemptId) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            debug_assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closing);
            debug_assert_eq!(runtime.open_attempt(), None);
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishOpenRejectedByClose {
                    attempt: attempt.get(),
                },
            );
        }
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        {
            let _ = (runtime, attempt);
        }
    }

    pub(crate) fn fail_open<A: Addin>(&self, runtime: &Runtime<A>, attempt: OpenAttemptId) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            debug_assert_eq!(runtime.open_attempt(), None);
            debug_assert!(matches!(
                runtime.phase(),
                crate::lifecycle::LifecyclePhase::OpenRollbackPending
                    | crate::lifecycle::LifecyclePhase::Closing
            ));
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::FailOpen {
                    attempt: attempt.get(),
                },
            );
        }
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        {
            let _ = (runtime, attempt);
        }
    }

    pub(crate) fn request_final_close<A: Addin>(&self, runtime: &Runtime<A>, recorded: &mut bool) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if !*recorded {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::RequestFinalClose,
            );
            *recorded = true;
        }
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        {
            let _ = (runtime, recorded);
        }
    }

    pub(crate) fn acquire_final_close_owner<A: Addin>(&self, runtime: &Runtime<A>) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::AcquireFinalCloseOwner,
        );
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        let _ = runtime;
    }

    pub(crate) fn acquire_open_rollback_owner<A: Addin>(&self, runtime: &Runtime<A>) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::AcquireOpenRollbackOwner,
        );
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        let _ = runtime;
    }

    pub(crate) fn release_cleanup_owner<A: Addin>(&self, runtime: &Runtime<A>) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::ReleaseCleanupOwner,
            );
            runtime.finish_composition_return();
        }
        #[cfg(not(any(test, feature = "shutdown-refinement")))]
        let _ = runtime;
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn retire_committed_shutdown<A: Addin>(&self, runtime: &Runtime<A>) {
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::RetireCommittedShutdown,
        );
    }
}
