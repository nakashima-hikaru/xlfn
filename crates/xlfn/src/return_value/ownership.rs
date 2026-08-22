#![allow(
    unused_imports,
    reason = "module boundary reexports are consumed through their parent"
)]

//! Excel-owned return-obligation ownership and quiescence accounting.

use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

const RETURN_STRIPE_COUNT: usize = 32;
const RETURN_STRIPE_SEALED: usize = 1_usize << (usize::BITS - 1);
const RETURN_STRIPE_COUNT_MASK: usize = RETURN_STRIPE_SEALED - 1;
thread_local! {
    static RETURN_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

static NEXT_RETURN_STRIPE: AtomicUsize = AtomicUsize::new(0);

fn current_return_stripe() -> usize {
    RETURN_STRIPE.with(|stripe| {
        let current = stripe.get();
        if current != usize::MAX {
            return current;
        }
        let assigned =
            NEXT_RETURN_STRIPE.fetch_add(1, Ordering::Relaxed) & (RETURN_STRIPE_COUNT - 1);
        stripe.set(assigned);
        assigned
    })
}

#[derive(Debug)]
#[repr(C, align(128))]
struct ReturnStripe {
    state: AtomicUsize,
}

impl ReturnStripe {
    const fn new_closed() -> Self {
        Self {
            state: AtomicUsize::new(RETURN_STRIPE_SEALED),
        }
    }

    fn try_enter(&self) -> bool {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & RETURN_STRIPE_SEALED != 0 {
                return false;
            }
            if observed & RETURN_STRIPE_COUNT_MASK == RETURN_STRIPE_COUNT_MASK {
                std::process::abort();
            }
            match self.state.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(current) => observed = current,
            }
        }
    }

    fn release(&self) -> bool {
        let previous = self.state.fetch_sub(1, Ordering::AcqRel);
        if previous & RETURN_STRIPE_COUNT_MASK == 0 {
            std::process::abort();
        }
        previous & RETURN_STRIPE_COUNT_MASK == 1
    }

    fn seal(&self) {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            if observed & RETURN_STRIPE_SEALED != 0 {
                return;
            }
            match self.state.compare_exchange_weak(
                observed,
                observed | RETURN_STRIPE_SEALED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }

    fn reopen(&self) {
        debug_assert!(self.state.load(Ordering::Acquire) & RETURN_STRIPE_SEALED != 0);
        debug_assert_eq!(self.active(), 0);
        self.state.store(0, Ordering::Release);
    }

    fn is_sealed(&self) -> bool {
        self.state.load(Ordering::Acquire) & RETURN_STRIPE_SEALED != 0
    }

    fn active(&self) -> usize {
        self.state.load(Ordering::Acquire) & RETURN_STRIPE_COUNT_MASK
    }
}

pub(crate) struct ReturnTracker {
    stripes: [ReturnStripe; RETURN_STRIPE_COUNT],
    wait_lock: Mutex<()>,
    quiescent: Condvar,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

pub(crate) struct ReturnObligation<'tracker> {
    stripe: &'tracker ReturnStripe,
    tracker: &'tracker ReturnTracker,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl<'tracker> ReturnObligation<'tracker> {
    pub(crate) fn tracker(&self) -> &'tracker ReturnTracker {
        self.tracker
    }
}

impl Drop for ReturnObligation<'_> {
    fn drop(&mut self) {
        if self.stripe.release() {
            // Synchronize the condition check and notification with the
            // waiter. Without this lock, the final release could notify just
            // before `wait_for_quiescence` goes to sleep.
            let _wait = self.tracker.wait_lock.lock();
            self.tracker.quiescent.notify_all();
        }
    }
}

impl ReturnTracker {
    pub(crate) const fn new_closed() -> Self {
        Self {
            stripes: [const { ReturnStripe::new_closed() }; RETURN_STRIPE_COUNT],
            wait_lock: Mutex::new(()),
            quiescent: Condvar::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.get() {
            ghost.record_event(event);
        }
    }

    pub(crate) fn reopen_admission(&self) -> XllResult<()> {
        if !self.admission_closed() || !self.is_quiescent() {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::RETURN_REOPEN,
            });
        }
        for stripe in &self.stripes {
            stripe.reopen();
        }
        Ok(())
    }

    pub(crate) fn close_admission(&self) {
        for stripe in &self.stripes {
            stripe.seal();
        }
    }

    pub(crate) fn try_enter_producer(&self) -> Option<ReturnProducerGuard<'_>> {
        let stripe_index = current_return_stripe();
        if !self.stripes[stripe_index].try_enter() {
            return None;
        }
        Some(ReturnProducerGuard {
            obligation: Some(ReturnObligation {
                stripe: &self.stripes[stripe_index],
                tracker: self,
            }),
        })
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        self.stripes.iter().all(|stripe| stripe.active() == 0)
    }

    pub(crate) fn admission_closed(&self) -> bool {
        self.stripes.iter().all(|stripe| stripe.is_sealed())
    }

    pub(crate) fn wait_for_quiescence(&self) {
        debug_assert!(self.admission_closed());

        let mut wait = self.wait_lock.lock();
        while !self.is_quiescent() {
            self.quiescent.wait(&mut wait);
        }
    }

    #[cfg(test)]
    pub(crate) fn outstanding_obligations(&self) -> usize {
        self.stripes.iter().map(|stripe| stripe.active()).sum()
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
        #[cfg(any(test, feature = "shutdown-refinement"))]
        {
            let _obligation = self
                .obligation
                .as_ref()
                .expect("return obligation is transferred exactly once");

            _obligation
                .tracker()
                .record_ghost_event(crate::shutdown_refinement::GhostEvent::CreateReturnBlock);
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
pub struct ReturnFreeBoundaryGuard {
    pub(crate) _operation: Option<ReturnFreeGuard>,
}

impl Drop for ReturnFreeGuard {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.obligation
            .tracker()
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::EndReturnFree);
    }
}
