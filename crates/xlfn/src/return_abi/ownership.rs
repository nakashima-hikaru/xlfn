//! Excel-owned return-obligation ownership and quiescence accounting.

use crate::XllResult;
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
            return Err(crate::error::construct::internal(
                crate::diagnostics::catalog::RETURN_REOPEN,
            ));
        }
        self.gate
            .control()
            .wait_if_sealed()
            .map(|closed| drop(closed.reopen()))
            .map_err(|_| {
                crate::error::construct::internal(crate::diagnostics::catalog::RETURN_REOPEN)
            })
    }

    pub(crate) fn close_admission(&self) {
        drop(self.gate.control().seal());
    }

    pub(crate) fn try_enter_producer(&self) -> Option<ReturnProducerGuard<'_>> {
        let stripe_index = current_return_stripe();
        let permit = self.gate.try_enter(stripe_index).ok()?;
        Some(ReturnProducerGuard {
            obligation: ReturnObligation {
                _permit: permit,
                observer: &self.observer,
            },
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
        drop(self.gate.control().wait_idle());
    }

    #[cfg(not(miri))]
    pub(crate) fn wait_for_quiescence_timeout(&self, timeout: std::time::Duration) -> bool {
        debug_assert!(self.admission_closed());
        let (_control, idle) = self.gate.control().wait_idle_timeout(timeout);
        idle
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn outstanding_obligations(&self) -> usize {
        self.gate.active()
    }
}

/// One admitted ownership obligation; publication consumes it exactly once.
pub(crate) struct ReturnProducerGuard<'tracker> {
    obligation: ReturnObligation<'tracker>,
}

impl ReturnProducerGuard<'static> {
    pub(crate) fn transfer_to_block(self) -> ReturnObligation<'static> {
        self.obligation.observe_create_block();
        self.obligation
    }
}

/// Retains the one admission until conversion and call postprocessing finish.
/// Publication consumes this owner; fallback never receives a used producer.
pub(super) struct ReturnPublicationTxn {
    producer: ReturnProducerGuard<'static>,
}

impl ReturnPublicationTxn {
    pub(super) fn new(producer: ReturnProducerGuard<'static>) -> Self {
        Self { producer }
    }

    pub(super) fn prepare(self, value: super::PreparedReturn) -> super::PreparedPublication {
        super::PreparedPublication {
            value,
            producer: self.producer,
        }
    }
}

/// No fallback owns the producer once publication starts. Stop on unwind
/// before an enclosing ABI catch could publish a second value.
#[inline]
pub(super) fn commit_publication<T>(operation: impl FnOnce() -> T) -> T {
    let committing = std::mem::DropGuard::new((), |()| xlfn_kernel::invariant::fail_stop());
    let publication = operation();
    std::mem::DropGuard::dismiss(committing);
    publication
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
