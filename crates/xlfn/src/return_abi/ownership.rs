#![allow(
    unused_imports,
    reason = "module boundary reexports are consumed through their parent"
)]

//! Excel-owned return-obligation ownership and quiescence accounting.

use crate::{XllError, XllResult};
use xlfn_kernel::drain_gate::{
    DEFAULT_STRIPE_COUNT, StripedDrainGate, StripedDrainPermit, current_thread_stripe,
};

const RETURN_STRIPE_COUNT: usize = DEFAULT_STRIPE_COUNT;

#[inline]
fn current_return_stripe() -> usize {
    current_thread_stripe()
}

pub(crate) struct ReturnTracker {
    gate: StripedDrainGate<RETURN_STRIPE_COUNT>,
    observer: ReturnObserver,
}

struct ReturnObserver {
    #[cfg(any(test, feature = "refinement"))]
    trace: std::sync::OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
}

pub(crate) struct ReturnObligation<'tracker> {
    _permit: StripedDrainPermit<'tracker, RETURN_STRIPE_COUNT>,
    observer: &'tracker ReturnObserver,
}

impl<'tracker> ReturnObligation<'tracker> {
    fn observe_create_block(&self) {
        self.observer.create_block();
    }

    pub(crate) fn observe_begin_free(&self) {
        self.observer.begin_free();
    }

    pub(crate) fn observe_release_block(&self) {
        self.observer.release_block();
    }

    fn observe_end_free(&self) {
        self.observer.end_free();
    }
}

impl ReturnTracker {
    pub(crate) const fn new_closed() -> Self {
        Self {
            gate: StripedDrainGate::new_sealed(),
            observer: ReturnObserver::new(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.observer.set_trace_sink(trace);
    }
}

impl ReturnObserver {
    const fn new() -> Self {
        Self {
            #[cfg(any(test, feature = "refinement"))]
            trace: std::sync::OnceLock::new(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        let _ = self.trace.set(trace);
    }

    fn record(&self, event: crate::shutdown_trace::ShutdownEvent) {
        #[cfg(any(test, feature = "refinement"))]
        if let Some(trace) = self.trace.get() {
            trace.record(event);
        }
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = event;
    }

    fn create_block(&self) {
        self.record(crate::shutdown_trace::ShutdownEvent::CreateReturnBlock);
    }

    fn begin_free(&self) {
        self.record(crate::shutdown_trace::ShutdownEvent::BeginReturnFree);
    }

    fn release_block(&self) {
        self.record(crate::shutdown_trace::ShutdownEvent::ReleaseReturnBlock);
    }

    fn end_free(&self) {
        self.record(crate::shutdown_trace::ShutdownEvent::EndReturnFree);
    }
}

impl ReturnTracker {
    pub(crate) fn reopen_admission(&self) -> XllResult<()> {
        if !self.admission_closed() || !self.is_quiescent() {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::RETURN_REOPEN,
            });
        }
        self.gate.reopen().map_err(|_| XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RETURN_REOPEN,
        })
    }

    pub(crate) fn close_admission(&self) {
        self.gate.seal();
    }

    pub(crate) fn try_enter_producer(&self) -> Option<ReturnProducerGuard<'_>> {
        let stripe_index = current_return_stripe();
        let permit = self.gate.try_enter(stripe_index).ok()?;
        Some(ReturnProducerGuard {
            obligation: Some(ReturnObligation {
                _permit: permit,
                observer: &self.observer,
            }),
        })
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        self.gate.active() == 0
    }

    pub(crate) fn admission_closed(&self) -> bool {
        self.gate.is_sealed()
    }

    pub(crate) fn wait_for_quiescence(&self) {
        debug_assert!(self.admission_closed());
        self.gate.wait_until_idle();
    }

    #[cfg(test)]
    pub(crate) fn outstanding_obligations(&self) -> usize {
        self.gate.active()
    }
}

pub(crate) struct ReturnProducerGuard<'tracker> {
    pub(crate) obligation: Option<ReturnObligation<'tracker>>,
}

impl<'tracker> ReturnProducerGuard<'tracker> {
    pub(crate) fn is_armed(&self) -> bool {
        self.obligation.is_some()
    }
}

impl ReturnProducerGuard<'static> {
    pub(crate) fn transfer_to_block(&mut self) -> ReturnObligation<'static> {
        let obligation = self
            .obligation
            .take()
            .expect("return obligation is transferred exactly once");
        obligation.observe_create_block();
        obligation
    }
}

pub(crate) struct ReturnFreeGuard {
    #[allow(
        dead_code,
        reason = "RAII obligation field held for lifecycle accounting"
    )]
    pub(crate) obligation: ReturnObligation<'static>,
}

/// Keeps one generated `xlAutoFree12` callback visible to terminal removal.
#[doc(hidden)]
pub(crate) struct ReturnFreeBoundaryGuard {
    pub(crate) _operation: Option<ReturnFreeGuard>,
}

impl Drop for ReturnFreeGuard {
    fn drop(&mut self) {
        self.obligation.observe_end_free();
    }
}
