#![allow(
    unused_imports,
    reason = "module boundary reexports are consumed through their parent"
)]

//! Excel-owned return-obligation ownership and quiescence accounting.

use crate::{XllError, XllResult};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn_kernel::drain_gate::{StripedDrainGate, StripedDrainPermit};

const RETURN_STRIPE_COUNT: usize = 32;
thread_local! {
    static RETURN_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

static NEXT_RETURN_STRIPE: AtomicUsize = AtomicUsize::new(0);

fn current_return_stripe() -> usize {
    let current = RETURN_STRIPE.get();
    if current != usize::MAX {
        return current;
    }
    let assigned = NEXT_RETURN_STRIPE.fetch_add(1, Ordering::Relaxed) & (RETURN_STRIPE_COUNT - 1);
    RETURN_STRIPE.set(assigned);
    assigned
}

pub(crate) struct ReturnTracker {
    gate: StripedDrainGate<RETURN_STRIPE_COUNT>,
    #[cfg(any(test, feature = "refinement"))]
    trace: std::sync::OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
}

pub(crate) struct ReturnObligation<'tracker> {
    _permit: StripedDrainPermit<'tracker, RETURN_STRIPE_COUNT>,
    #[cfg(any(test, feature = "refinement"))]
    tracker: &'tracker ReturnTracker,
}

#[cfg(any(test, feature = "refinement"))]
impl<'tracker> ReturnObligation<'tracker> {
    pub(crate) fn tracker(&self) -> &'tracker ReturnTracker {
        self.tracker
    }
}

impl ReturnTracker {
    pub(crate) const fn new_closed() -> Self {
        Self {
            gate: StripedDrainGate::new_sealed(),
            #[cfg(any(test, feature = "refinement"))]
            trace: std::sync::OnceLock::new(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        let _ = self.trace.set(trace);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_shutdown_event(&self, event: crate::shutdown_trace::ShutdownEvent) {
        if let Some(trace) = self.trace.get() {
            trace.record(event);
        }
    }

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
                #[cfg(any(test, feature = "refinement"))]
                tracker: self,
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
        #[cfg(any(test, feature = "refinement"))]
        {
            let _obligation = self
                .obligation
                .as_ref()
                .expect("return obligation is transferred exactly once");

            _obligation
                .tracker()
                .record_shutdown_event(crate::shutdown_trace::ShutdownEvent::CreateReturnBlock);
        }

        self.obligation
            .take()
            .expect("return obligation is transferred exactly once")
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
        #[cfg(any(test, feature = "refinement"))]
        self.obligation
            .tracker()
            .record_shutdown_event(crate::shutdown_trace::ShutdownEvent::EndReturnFree);
    }
}
