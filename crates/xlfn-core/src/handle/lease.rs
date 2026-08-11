use super::*;

pub(crate) struct HandleLeaseState {
    pub(crate) active: AtomicUsize,
    pub(crate) waiters: AtomicUsize,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
    pub(crate) cleanup_failure: Mutex<Option<XllError>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    pub(crate) before_idle_wait_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl HandleLeaseState {
    pub(crate) fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
            cleanup_failure: Mutex::new(None),
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
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle lease count cannot overflow");

        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginHandleOperation);

        HandleLease {
            state: Arc::clone(self),
        }
    }

    pub(crate) fn wait_for_idle(&self) {
        let mut guard = self.wait_lock.lock();
        self.waiters.fetch_add(1, Ordering::AcqRel);

        while self.active.load(Ordering::Acquire) != 0 {
            #[cfg(test)]
            if let Some(hook) = self.before_idle_wait_hook.lock().as_ref().cloned() {
                hook();
            }
            self.idle.wait(&mut guard);
        }

        let previous = self.waiters.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }

    pub(crate) fn record_cleanup_failure(&self, error: XllError) {
        let mut failure = self.cleanup_failure.lock();
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        let failure = self.cleanup_failure.lock();
        match failure.as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

pub(crate) struct HandleLease {
    pub(crate) state: Arc<HandleLeaseState>,
}

impl HandleLease {
    pub(crate) fn record_cleanup_failure(&self, error: XllError) {
        self.state.record_cleanup_failure(error);
    }
}

impl Clone for HandleLease {
    fn clone(&self) -> Self {
        self.state.acquire()
    }
}

impl Drop for HandleLease {
    fn drop(&mut self) {
        let previous = self
            .state
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("handle lease count remains balanced");

        if previous == 1 && self.state.waiters.load(Ordering::Acquire) != 0 {
            let _wait_guard = self.state.wait_lock.lock();

            if self.state.active.load(Ordering::Acquire) == 0 {
                self.state.idle.notify_all();
            }
        }

        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.state
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::EndHandleOperation);
    }
}

pub(crate) struct HandlePrepareState {
    pub(crate) active: AtomicUsize,
    pub(crate) waiters: AtomicUsize,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
}

impl HandlePrepareState {
    pub(crate) const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    pub(crate) fn enter(&self) -> HandlePrepareGuard<'_> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle prepare count cannot overflow");

        HandlePrepareGuard { state: self }
    }

    pub(crate) fn wait_for_idle(&self) {
        let mut guard = self.wait_lock.lock();
        self.waiters.fetch_add(1, Ordering::AcqRel);

        while self.active.load(Ordering::Acquire) != 0 {
            self.idle.wait(&mut guard);
        }

        let previous = self.waiters.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

pub(crate) struct HandlePrepareGuard<'a> {
    pub(crate) state: &'a HandlePrepareState,
}

impl Drop for HandlePrepareGuard<'_> {
    fn drop(&mut self) {
        let previous = self.state.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);

        if previous != 1 || self.state.waiters.load(Ordering::Acquire) == 0 {
            return;
        }

        let _guard = self.state.wait_lock.lock();

        if self.state.active.load(Ordering::Acquire) == 0 {
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
