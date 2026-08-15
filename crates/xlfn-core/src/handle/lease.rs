use super::*;
use std::cell::Cell;
use std::time::Duration;

const HANDLE_LEASE_STRIPE_COUNT: usize = 32;
const HANDLE_LEASE_STRIPE_MASK: usize = HANDLE_LEASE_STRIPE_COUNT - 1;
const HANDLE_LEASE_QUIESCENCE_RECHECK_INTERVAL: Duration = Duration::from_millis(1);
const HANDLE_PREPARE_STRIPE_COUNT: usize = 32;
const HANDLE_PREPARE_STRIPE_MASK: usize = HANDLE_PREPARE_STRIPE_COUNT - 1;
const HANDLE_PREPARE_QUIESCENCE_RECHECK_INTERVAL: Duration = Duration::from_millis(1);

thread_local! {
    static HANDLE_LEASE_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
    static HANDLE_PREPARE_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

static NEXT_HANDLE_LEASE_STRIPE: AtomicUsize = AtomicUsize::new(0);
static NEXT_HANDLE_PREPARE_STRIPE: AtomicUsize = AtomicUsize::new(0);

fn current_handle_lease_stripe() -> usize {
    HANDLE_LEASE_STRIPE.with(|stripe| {
        let current = stripe.get();
        if current != usize::MAX {
            return current;
        }
        let assigned =
            NEXT_HANDLE_LEASE_STRIPE.fetch_add(1, Ordering::Relaxed) & HANDLE_LEASE_STRIPE_MASK;
        stripe.set(assigned);
        assigned
    })
}

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

#[derive(Debug)]
#[repr(C, align(128))]
struct HandleLeaseStripe {
    active: AtomicUsize,
    cleanup_failure: Mutex<Option<XllError>>,
}

impl HandleLeaseStripe {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            cleanup_failure: Mutex::new(None),
        }
    }

    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

pub(crate) struct HandleLeaseState {
    stripes: [Arc<HandleLeaseStripe>; HANDLE_LEASE_STRIPE_COUNT],
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    pub(crate) before_idle_wait_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl HandleLeaseState {
    pub(crate) fn new() -> Self {
        Self {
            stripes: std::array::from_fn(|_| Arc::new(HandleLeaseStripe::new())),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(test)]
            before_idle_wait_hook: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(event);
        }
    }

    pub(crate) fn acquire(self: &Arc<Self>) -> HandleLease {
        let stripe = Arc::clone(&self.stripes[current_handle_lease_stripe()]);
        stripe
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle lease count cannot overflow");

        #[cfg(any(test, feature = "shutdown-refinement"))]
        let ghost = self.ghost.lock().clone();

        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::BeginHandleOperation);
        }

        HandleLease {
            stripe,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost,
        }
    }

    pub(crate) fn wait_for_idle(&self) {
        while self.active() != 0 {
            #[cfg(test)]
            if let Some(hook) = self.before_idle_wait_hook.lock().as_ref().cloned() {
                hook();
            }
            std::thread::sleep(HANDLE_LEASE_QUIESCENCE_RECHECK_INTERVAL);
        }
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        for stripe in &self.stripes {
            let failure = stripe.cleanup_failure.lock();
            if let Some(error) = failure.as_ref() {
                return Err(error.clone());
            }
        }
        Ok(())
    }

    pub(crate) fn active(&self) -> usize {
        self.stripes.iter().map(|stripe| stripe.active()).sum()
    }
}

pub(crate) struct HandleLease {
    stripe: Arc<HandleLeaseStripe>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Option<crate::shutdown_refinement::GhostHandle>,
}

impl HandleLease {
    pub(crate) fn record_cleanup_failure(&self, error: XllError) {
        let mut failure = self.stripe.cleanup_failure.lock();
        if failure.is_none() {
            *failure = Some(error);
        }
    }
}

impl Clone for HandleLease {
    fn clone(&self) -> Self {
        self.stripe
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle lease count cannot overflow");

        #[cfg(any(test, feature = "shutdown-refinement"))]
        let ghost = self.ghost.clone();

        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::BeginHandleOperation);
        }

        Self {
            stripe: Arc::clone(&self.stripe),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost,
        }
    }
}

impl Drop for HandleLease {
    fn drop(&mut self) {
        self.stripe
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("handle lease count remains balanced");

        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::EndHandleOperation);
        }
    }
}

#[derive(Debug)]
#[repr(C, align(128))]
struct HandlePrepareStripe {
    active: AtomicUsize,
}

impl HandlePrepareStripe {
    const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
        }
    }

    fn enter(&self) {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle prepare count cannot overflow");
    }

    fn leave(&self) {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("handle prepare count remains balanced");
    }

    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

pub(crate) struct HandlePrepareState {
    stripes: [HandlePrepareStripe; HANDLE_PREPARE_STRIPE_COUNT],
    pub(crate) waiters: AtomicUsize,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
}

impl HandlePrepareState {
    pub(crate) const fn new() -> Self {
        Self {
            stripes: [const { HandlePrepareStripe::new() }; HANDLE_PREPARE_STRIPE_COUNT],
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    pub(crate) fn enter(&self) -> HandlePrepareGuard<'_> {
        let stripe_index = current_handle_prepare_stripe();
        self.stripes[stripe_index].enter();

        HandlePrepareGuard {
            state: self,
            stripe_index,
        }
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
        self.stripes.iter().map(HandlePrepareStripe::active).sum()
    }
}

pub(crate) struct HandlePrepareGuard<'a> {
    pub(crate) state: &'a HandlePrepareState,
    stripe_index: usize,
}

impl Drop for HandlePrepareGuard<'_> {
    fn drop(&mut self) {
        self.state.stripes[self.stripe_index].leave();

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
