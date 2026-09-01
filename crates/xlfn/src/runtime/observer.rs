//! Refinement instrumentation for the runtime protocol.
//!
//! The operational lifecycle transition is owned by `Runtime`; this façade
//! only observes that transition when the refinement model is enabled. Keeping
//! the build configuration here prevents production and refinement builds
//! from carrying separate state-transition implementations.

use crate::generation::{OpenAttemptId, RuntimeGeneration};
use crate::runtime::capabilities::OpenDeps;
use crate::runtime_components::ReturnProtocol;
#[cfg(not(any(test, feature = "refinement")))]
use std::marker::PhantomData;
#[cfg(any(test, feature = "refinement"))]
use std::sync::Arc;

pub(crate) struct RuntimeObserver {
    #[cfg(any(test, feature = "refinement"))]
    formal: crate::runtime_components::FormalState,
}

impl RuntimeObserver {
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(any(test, feature = "refinement"))]
            formal: crate::runtime_components::FormalState::new(),
        }
    }

    #[inline(always)]
    fn event(&self, event: crate::shutdown_trace::ShutdownEvent) {
        #[cfg(any(test, feature = "refinement"))]
        self.trace_handle().record(event);
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = event;
    }

    #[inline]
    pub(crate) fn async_stopped(&self) {
        // The async manager has already performed the concrete stop before
        // this observation is emitted.  The recorder must not infer whether
        // that transition happened from a shadow resource counter.
        self.event(crate::shutdown_trace::ShutdownEvent::StopAsyncExecutor);
    }

    pub(crate) fn diagnostics_stopped(&self) {
        self.event(crate::shutdown_trace::ShutdownEvent::StopDiagnostics);
    }

    #[allow(
        dead_code,
        reason = "The no-op production observer keeps one API across builds"
    )]
    pub(crate) fn mark_returned_success(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.trace_handle().mark_returned_success();
    }

    pub(crate) fn fail_stop(&self, reason: crate::shutdown_trace::ShutdownFailure) {
        self.event(crate::shutdown_trace::ShutdownEvent::FailStop(reason));
    }

    pub(crate) fn quarantine(&self, reason: crate::shutdown_trace::ShutdownFailure) {
        self.event(crate::shutdown_trace::ShutdownEvent::Quarantine(reason));
    }

    #[cfg(test)]
    pub(crate) fn disable_for_test(&self) {
        self.trace_handle().disable_for_test();
    }

    pub(crate) fn finish_close(&self) {
        self.event(crate::shutdown_trace::ShutdownEvent::FinishClose);
        #[cfg(any(test, feature = "refinement"))]
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::FinishCommittedShutdown,
        );
    }

    pub(crate) fn finish_open_rollback(&self) {
        #[cfg(any(test, feature = "refinement"))]
        {
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishOpenRollback(
                    crate::shutdown_trace::ShutdownResources::quiescent_snapshot(),
                ),
            );
            self.mark_composition_terminal_pending();
        }
    }

    pub(crate) fn publish_committed_closed(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::PublishCommittedClosed,
        );
    }

    pub(crate) fn finish_uncommitted_final_close(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::FinishUncommittedFinalClose(
                crate::shutdown_trace::ShutdownResources::quiescent_snapshot(),
            ),
        );
    }

    pub(crate) fn mark_return_pending(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.composition_trace().mark_return_pending();
    }

    pub(crate) fn finish_return(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.composition_trace().finish_return();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn trace_handle(&self) -> crate::shutdown_trace::ShutdownTraceHandle {
        Arc::clone(
            self.formal
                .trace
                .get_or_init(|| Arc::new(crate::shutdown_trace::ShutdownTraceRecorder::new())),
        )
    }

    pub(crate) fn observe_call(&self) -> CallObservation<'_> {
        #[cfg(any(test, feature = "refinement"))]
        {
            let id = crate::shutdown_trace::ActivityId::fresh();
            self.event(crate::shutdown_trace::ShutdownEvent::EnterCall { id });
            CallObservation { observer: self, id }
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            CallObservation {
                _observer: PhantomData,
            }
        }
    }

    pub(crate) fn observe_external(&self) -> ExternalObservation<'_> {
        #[cfg(any(test, feature = "refinement"))]
        {
            let id = crate::shutdown_trace::ActivityId::fresh();
            self.event(crate::shutdown_trace::ShutdownEvent::EnterExternal { id });
            ExternalObservation { observer: self, id }
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            ExternalObservation {
                _observer: PhantomData,
            }
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn composition_trace(&self) -> &crate::composition_refinement::CompositionTrace {
        let trace = self
            .formal
            .composition
            .get_or_init(|| Arc::new(crate::composition_refinement::CompositionTrace::new()));
        self.trace_handle().set_composition(Arc::clone(trace));
        trace.as_ref()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_composition_event(
        &self,
        event: crate::composition_refinement::CompositionEvent,
    ) {
        self.composition_trace().record(event);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn finish_composition_return(&self) {
        self.composition_trace().finish_return();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn mark_composition_terminal_pending(&self) {
        self.composition_trace().mark_terminal_pending();
    }

    pub(crate) fn begin_open(
        &self,
        returns: &ReturnProtocol,
        sampled_epoch: u64,
        attempt: OpenAttemptId,
    ) {
        #[cfg(any(test, feature = "refinement"))]
        {
            let trace = self.trace_handle();
            returns.returns.set_trace_sink(trace);
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::BeginOpen {
                    sampled_epoch,
                    attempt: attempt.get(),
                },
            );
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = (returns, sampled_epoch, attempt);
        }
    }

    pub(crate) fn commit_open<A: crate::Addin>(
        &self,
        deps: &OpenDeps<'_, A>,
        attempt: OpenAttemptId,
        _generation: RuntimeGeneration,
    ) {
        #[cfg(any(test, feature = "refinement"))]
        {
            let trace = self.trace_handle();
            let mut resources = crate::shutdown_trace::ShutdownResources::opened(
                deps.host().registrations_snapshot().len() as u64,
                deps.host().event_registrations_snapshot().len() as u64,
            );
            #[cfg(feature = "async")]
            {
                resources.async_executor_running = !deps.executors().async_manager.is_stopped();
            }
            let _ = crate::diagnostics::connect_trace(Arc::clone(&trace), |snapshot| {
                resources.diagnostics_running = snapshot.running;
                resources.diagnostics_pending = snapshot.pending;
                let _ = trace.begin(attempt.get(), resources.clone());
                Ok(())
            });
            crate::excel_rtd::set_trace_sink(Arc::clone(&trace));
            deps.returns().returns.set_trace_sink(Arc::clone(&trace));
            #[cfg(any(feature = "handles", feature = "rtd"))]
            deps.with_generation_services(|services| {
                #[cfg(feature = "handles")]
                services.set_handle_trace_sink(Arc::clone(&trace));
                #[cfg(feature = "rtd")]
                services.set_subscription_trace_sink(Arc::clone(&trace));
            })
            .expect("committed open generation publishes its services");
            #[cfg(feature = "async")]
            deps.executors()
                .async_manager
                .set_trace_sink(Arc::clone(&trace));
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::CommitOpen {
                    attempt: attempt.get(),
                    resources,
                },
            );
            let _ = _generation;
        }
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = (deps, attempt, _generation);
    }

    pub(crate) fn reject_open(&self, attempt: OpenAttemptId) {
        #[cfg(any(test, feature = "refinement"))]
        {
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::FinishOpenRejectedByClose {
                    attempt: attempt.get(),
                },
            );
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = attempt;
        }
    }

    pub(crate) fn fail_open(&self, attempt: OpenAttemptId) {
        #[cfg(any(test, feature = "refinement"))]
        {
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::FailOpen {
                    attempt: attempt.get(),
                },
            );
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = attempt;
        }
    }

    pub(crate) fn request_final_close(&self, recorded: &mut bool) {
        #[cfg(any(test, feature = "refinement"))]
        if !*recorded {
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::RequestFinalClose,
            );
            *recorded = true;
        }
        #[cfg(not(any(test, feature = "refinement")))]
        {
            let _ = recorded;
        }
    }

    pub(crate) fn acquire_final_close_owner(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::AcquireFinalCloseOwner,
        );
    }

    pub(crate) fn acquire_open_rollback_owner(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::AcquireOpenRollbackOwner,
        );
    }

    pub(crate) fn release_cleanup_owner(&self) {
        #[cfg(any(test, feature = "refinement"))]
        {
            self.record_composition_event(
                crate::composition_refinement::CompositionEvent::ReleaseCleanupOwner,
            );
            self.finish_composition_return();
        }
    }

    pub(crate) fn retire_committed_shutdown(&self) {
        #[cfg(any(test, feature = "refinement"))]
        self.record_composition_event(
            crate::composition_refinement::CompositionEvent::RetireCommittedShutdown,
        );
    }
}

macro_rules! runtime_shutdown_events {
    ($($name:ident => $event:ident),+ $(,)?) => {
        impl RuntimeObserver {
            $(
                pub(crate) fn $name(&self) {
                    self.event(crate::shutdown_trace::ShutdownEvent::$event);
                }
            )+
        }
    };
}

runtime_shutdown_events! {
    begin_close => BeginClose,
    returns_drained => ReturnsDrained,
    async_drained => AsyncDrained,
    subscriptions_drained => SubscriptionsDrained,
    unregister_function => UnregisterFunction,
    unregister_event => UnregisterEvent,
    callback_admission_closed => CloseCallbackGate,
    host_detached => HostDetached,
    generation_unique => ProveGenerationUnique,
    addin_quiesced => ProveAddinQuiesced,
    generation_reclaimed => GenerationReclaimed,
    cleanup_issue => RecordCleanupIssue,
    handles_drained => HandlesDrained,
    diagnostics_drained => DiagnosticsDrained,
    rtd_drained => RtdDrained,
}

impl RuntimeObserver {
    pub(crate) fn calls_drained(&self) {
        self.event(crate::shutdown_trace::ShutdownEvent::CallsDrained);
    }
}

pub(crate) struct CallObservation<'a> {
    #[cfg(any(test, feature = "refinement"))]
    observer: &'a RuntimeObserver,
    #[cfg(any(test, feature = "refinement"))]
    id: crate::shutdown_trace::ActivityId,
    #[cfg(not(any(test, feature = "refinement")))]
    _observer: PhantomData<&'a RuntimeObserver>,
}

impl Drop for CallObservation<'_> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "refinement"))]
        self.observer
            .event(crate::shutdown_trace::ShutdownEvent::LeaveCall { id: self.id });
    }
}

pub(crate) struct ExternalObservation<'a> {
    #[cfg(any(test, feature = "refinement"))]
    observer: &'a RuntimeObserver,
    #[cfg(any(test, feature = "refinement"))]
    id: crate::shutdown_trace::ActivityId,
    #[cfg(not(any(test, feature = "refinement")))]
    _observer: PhantomData<&'a RuntimeObserver>,
}

impl Drop for ExternalObservation<'_> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "refinement"))]
        self.observer
            .event(crate::shutdown_trace::ShutdownEvent::LeaveExternal { id: self.id });
    }
}
