//! Refinement instrumentation for the runtime protocol.
//!
//! The operational lifecycle transition is owned by `Runtime`; this façade
//! only observes or augments that transition when the refinement model is
//! enabled. Keeping the build configuration here prevents production and
//! refinement builds from carrying separate state-transition implementations.

#[cfg(any(test, feature = "refinement"))]
use crate::XllError;
use crate::generation::OpenAttemptId;
#[cfg(any(test, feature = "refinement"))]
use crate::runtime::ClosedWitness;
use crate::runtime::Runtime;
use crate::{Addin, XllResult};
#[cfg(any(test, feature = "refinement"))]
use std::sync::Arc;

pub(crate) struct RuntimeRefinementHooks {
    #[cfg(any(test, feature = "refinement"))]
    formal: crate::runtime_components::FormalState,
}

impl RuntimeRefinementHooks {
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(any(test, feature = "refinement"))]
            formal: crate::runtime_components::FormalState::new(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    #[inline]
    pub(crate) fn event<A: Addin>(
        &self,
        runtime: &Runtime<A>,
        event: crate::shutdown_refinement::GhostEvent,
    ) {
        self.ghost_handle().record_event(event);
        let _ = runtime;
    }

    #[cfg(any(test, feature = "refinement"))]
    #[inline]
    pub(crate) fn event_linearized<A: Addin>(
        &self,
        runtime: &Runtime<A>,
        event: crate::shutdown_refinement::GhostEvent,
    ) {
        crate::module_runtime::ingress().with_linearization(|| self.event(runtime, event));
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn generation_active<A: Addin>(&self, runtime: &Runtime<A>) -> bool {
        let _ = runtime;
        self.ghost_handle().active()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn async_stopped<A: Addin>(&self, runtime: &Runtime<A>) {
        let ghost = self.ghost_handle();
        if ghost.state().resources.async_executor_running {
            self.event(
                runtime,
                crate::shutdown_refinement::GhostEvent::StopAsyncExecutor,
            );
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn diagnostics_stopped<A: Addin>(&self, runtime: &Runtime<A>) -> XllResult<()> {
        let _ = runtime;
        crate::diagnostics::record_ghost_diagnostics_stopped(self.ghost_handle())
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn fail_stop<A: Addin>(
        &self,
        runtime: &Runtime<A>,
        reason: crate::shutdown_refinement::GhostFailure,
    ) {
        if let Err(violation) = self.ghost_handle().fail_stop(reason) {
            tracing::error!(%violation, "shutdown ghost fail-stop recording failed");
        }
        let _ = runtime;
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn quarantine<A: Addin>(
        &self,
        runtime: &Runtime<A>,
        reason: crate::shutdown_refinement::GhostFailure,
    ) {
        if let Err(violation) = self.ghost_handle().quarantine(reason) {
            tracing::error!(%violation, "shutdown ghost quarantine recording failed");
        }
        let _ = runtime;
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_returned_success<A: Addin>(
        &self,
        runtime: &Runtime<A>,
        witness: &ClosedWitness,
    ) -> XllResult<()> {
        if witness.runtime_address != std::ptr::from_ref(runtime).addr()
            || witness.generation != runtime.last_committed_generation()
            || runtime.phase() != crate::lifecycle::LifecyclePhase::Closed
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_WAIT,
            });
        }
        if self.ghost_handle().active() {
            self.ghost_handle()
                .record_returned_success()
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RTD_SUBSCRIPTION,
                })?;
            if self.ghost_handle().active() {
                crate::lifecycle::fail_stop_invariant(
                    "xlAutoRemove ghost shutdown postcondition",
                    &XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RTD_SUBSCRIPTION,
                    },
                );
            }
            self.retire_committed_shutdown(runtime);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn disable_for_test(&self) {
        self.ghost_handle().disable_for_test();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn finish_close<A: Addin>(&self, runtime: &Runtime<A>) -> XllResult<()> {
        if self.ghost_handle().active() {
            self.ghost_handle()
                .apply(crate::shutdown_refinement::GhostEvent::FinishClose)
                .map_err(|_| XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_GHOST,
                })?;
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishCommittedShutdown,
            );
        }
        Ok(())
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn ghost_handle(&self) -> crate::shutdown_refinement::GhostHandle {
        Arc::clone(
            self.formal
                .ghost
                .get_or_init(|| Arc::new(crate::shutdown_refinement::ShutdownGhost::new())),
        )
    }

    #[cfg(any(test, feature = "refinement"))]
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
        #[cfg(any(test, feature = "refinement"))]
        {
            let ghost = runtime.refinement_hooks().ghost_handle();
            if ghost.active() {
                runtime.return_protocol.returns.set_ghost(ghost);
            }
            runtime.record_composition_begin_open(sampled_epoch, attempt.get());
        }
        #[cfg(not(any(test, feature = "refinement")))]
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
        #[cfg(any(test, feature = "refinement"))]
        {
            let ghost = runtime.refinement_hooks().ghost_handle();
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
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::GHOST_GENERATION,
                    })?;
                operation()
            })?;
            crate::rtd::set_ghost(Arc::clone(&ghost));
            runtime
                .return_protocol
                .returns
                .set_ghost(Arc::clone(&ghost));
            let services = runtime
                .generation_services()
                .expect("committed open generation publishes its services");
            services.formula_handle_slot().set_ghost(Arc::clone(&ghost));
            services.subscriptions_slot().set_ghost(Arc::clone(&ghost));
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
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = (runtime, attempt);
            operation()
        }
    }

    pub(crate) fn reject_open<A: Addin>(&self, runtime: &Runtime<A>, attempt: OpenAttemptId) {
        #[cfg(any(test, feature = "refinement"))]
        {
            debug_assert_eq!(runtime.phase(), crate::lifecycle::LifecyclePhase::Closing);
            debug_assert_eq!(runtime.open_attempt(), None);
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishOpenRejectedByClose {
                    attempt: attempt.get(),
                },
            );
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = (runtime, attempt);
        }
    }

    pub(crate) fn fail_open<A: Addin>(&self, runtime: &Runtime<A>, attempt: OpenAttemptId) {
        #[cfg(any(test, feature = "refinement"))]
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
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = (runtime, attempt);
        }
    }

    pub(crate) fn request_final_close<A: Addin>(&self, runtime: &Runtime<A>, recorded: &mut bool) {
        #[cfg(any(test, feature = "refinement"))]
        if !*recorded {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::RequestFinalClose,
            );
            *recorded = true;
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = (runtime, recorded);
        }
    }

    pub(crate) fn acquire_final_close_owner<A: Addin>(&self, runtime: &Runtime<A>) {
        #[cfg(any(test, feature = "refinement"))]
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::AcquireFinalCloseOwner,
        );
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = runtime;
    }

    pub(crate) fn acquire_open_rollback_owner<A: Addin>(&self, runtime: &Runtime<A>) {
        #[cfg(any(test, feature = "refinement"))]
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::AcquireOpenRollbackOwner,
        );
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = runtime;
    }

    pub(crate) fn release_cleanup_owner<A: Addin>(&self, runtime: &Runtime<A>) {
        #[cfg(any(test, feature = "refinement"))]
        {
            runtime.record_composition_event(
                crate::composition_refinement::CompositionEvent::ReleaseCleanupOwner,
            );
            runtime.finish_composition_return();
        }
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = runtime;
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn retire_committed_shutdown<A: Addin>(&self, runtime: &Runtime<A>) {
        runtime.record_composition_event(
            crate::composition_refinement::CompositionEvent::RetireCommittedShutdown,
        );
    }
}

macro_rules! runtime_ghost_events {
    ($($name:ident => $event:ident),+ $(,)?) => {
        impl RuntimeRefinementHooks {
            $(
                #[cfg(any(test, feature = "refinement"))]
                pub(crate) fn $name<A: Addin>(&self, runtime: &Runtime<A>) {
                    self.event(runtime, crate::shutdown_refinement::GhostEvent::$event);
                }
            )+
        }
    };
}

runtime_ghost_events! {
    external_entered => EnterExternal,
    external_left => LeaveExternal,
    call_entered => EnterCall,
    call_left => LeaveCall,
    begin_close => BeginClose,
    returns_drained => ReturnsDrained,
    async_drained => AsyncDrained,
    subscriptions_drained => SubscriptionsDrained,
    unregister_function => UnregisterFunction,
    unregister_event => UnregisterEvent,
    callback_admission_closed => CloseCallbackAdmission,
    host_detached => HostDetached,
    generation_unique => ProveGenerationUnique,
    addin_quiesced => ProveAddinQuiesced,
    generation_reclaimed => GenerationReclaimed,
    cleanup_issue => RecordCleanupIssue,
    handles_drained => HandlesDrained,
    diagnostics_drained => DiagnosticsDrained,
    rtd_drained => RtdDrained,
}

impl RuntimeRefinementHooks {
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn calls_drained<A: Addin>(&self, runtime: &Runtime<A>) {
        self.event_linearized(
            runtime,
            crate::shutdown_refinement::GhostEvent::CallsDrained,
        );
    }
}
