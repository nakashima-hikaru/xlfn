use super::*;

pub(crate) struct OperationGate {
    pub(crate) state: AtomicUsize,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
}

pub(crate) const CLOSING_BIT: usize = usize::MAX / 2 + 1;

impl OperationGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        })
    }

    pub(crate) fn enter(self: &Arc<Self>) -> XllResult<OperationGuard> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |val| {
                if (val & CLOSING_BIT) != 0 {
                    None
                } else {
                    Some(val + 1)
                }
            })
            .map_err(|_| XllError::Closing)?;

        Ok(OperationGuard {
            gate: Arc::clone(self),
        })
    }

    pub(crate) fn close_and_wait_begin(&self) -> TerminationWaitGuard<'_> {
        self.state.fetch_or(CLOSING_BIT, Ordering::AcqRel);
        TerminationWaitGuard { gate: self }
    }

    pub(crate) fn leave(&self) {
        let prev = self.state.fetch_sub(1, Ordering::AcqRel);
        let active_count = (prev & !CLOSING_BIT) - 1;
        if active_count == 0 && (prev & CLOSING_BIT) != 0 {
            let _guard = self.wait_lock.lock();
            self.idle.notify_all();
        }
    }
}

pub(crate) struct OperationGuard {
    pub(crate) gate: Arc<OperationGate>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.gate.leave();
    }
}

pub(crate) struct TerminationWaitGuard<'a> {
    pub(crate) gate: &'a OperationGate,
}

impl TerminationWaitGuard<'_> {
    pub(crate) fn wait(self) {
        let mut guard = self.gate.wait_lock.lock();
        while (self.gate.state.load(Ordering::Acquire) & !CLOSING_BIT) > 0 {
            self.gate.idle.wait(&mut guard);
        }
    }
}
