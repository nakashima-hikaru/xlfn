use crossbeam_utils::CachePadded;
use parking_lot::{Condvar, Mutex};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const HANDLE_PREPARE_STRIPE_COUNT: usize = 32;
const HANDLE_PREPARE_STRIPE_MASK: usize = HANDLE_PREPARE_STRIPE_COUNT - 1;
const HANDLE_PREPARE_QUIESCENCE_RECHECK_INTERVAL: Duration = Duration::from_millis(1);

thread_local! {
    static HANDLE_PREPARE_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

static NEXT_HANDLE_PREPARE_STRIPE: AtomicUsize = AtomicUsize::new(0);

fn current_handle_prepare_stripe() -> usize {
    HANDLE_PREPARE_STRIPE.with(|stripe| {
        let current = stripe.get();
        if current != usize::MAX {
            return current;
        }
        let assigned =
            NEXT_HANDLE_PREPARE_STRIPE.fetch_add(1, Ordering::Relaxed) & HANDLE_PREPARE_STRIPE_MASK;
        stripe.set(assigned);
        assigned
    })
}

pub(crate) struct HandlePrepareState {
    accepting: AtomicBool,
    stripes: [CachePadded<AtomicUsize>; HANDLE_PREPARE_STRIPE_COUNT],
    pub(crate) waiters: AtomicUsize,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
}

impl HandlePrepareState {
    pub(crate) const fn new() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            stripes: [const { CachePadded::new(AtomicUsize::new(0)) }; HANDLE_PREPARE_STRIPE_COUNT],
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    pub(crate) fn try_enter(&self) -> Option<HandlePrepareGuard<'_>> {
        if !self.accepting.load(Ordering::Acquire) {
            return None;
        }
        let stripe_index = current_handle_prepare_stripe();
        self.stripes[stripe_index]
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle prepare count cannot overflow");

        let guard = HandlePrepareGuard {
            state: self,
            stripe_index,
        };
        if self.accepting.load(Ordering::Acquire) {
            Some(guard)
        } else {
            drop(guard);
            None
        }
    }

    pub(crate) fn close_admission(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    pub(crate) fn wait_for_idle(&self) {
        let mut guard = self.wait_lock.lock();
        self.waiters.fetch_add(1, Ordering::AcqRel);

        while self.active() != 0 {
            self.idle
                .wait_for(&mut guard, HANDLE_PREPARE_QUIESCENCE_RECHECK_INTERVAL);
        }

        let previous = self.waiters.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }

    fn active(&self) -> usize {
        self.stripes
            .iter()
            .map(|stripe| stripe.load(Ordering::Acquire))
            .sum()
    }
}

pub(crate) struct HandlePrepareGuard<'a> {
    pub(crate) state: &'a HandlePrepareState,
    stripe_index: usize,
}

impl Drop for HandlePrepareGuard<'_> {
    fn drop(&mut self) {
        self.state.stripes[self.stripe_index]
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("handle prepare count remains balanced");

        if self.state.waiters.load(Ordering::Acquire) == 0 || self.state.active() != 0 {
            return;
        }

        let _guard = self.state.wait_lock.lock();

        if self.state.active() == 0 {
            self.state.idle.notify_all();
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) struct RtdOperationGuard {
    pub(crate) _ingress_guard: Option<crate::ingress::ExportCallGuard<'static>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Option<crate::shutdown_refinement::GhostHandle>,
}

#[cfg(target_os = "windows")]
impl Drop for RtdOperationGuard {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_admission_rejects_new_prepares_and_drains_existing_ones() {
        let state = HandlePrepareState::new();
        let active = state.try_enter().expect("prepare admission starts open");

        state.close_admission();
        assert!(state.try_enter().is_none());

        drop(active);
        state.wait_for_idle();
    }
}
