use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct OperationGate {
    pub(crate) state: AtomicUsize,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
}

pub(crate) const CLOSING_BIT: usize = usize::MAX / 2 + 1;

impl Default for OperationGate {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationGate {
    pub(crate) fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    #[inline]
    pub(crate) fn is_closing(&self) -> bool {
        (self.state.load(Ordering::Acquire) & CLOSING_BIT) != 0
    }

    #[inline]
    pub(crate) fn begin_close(&self) {
        self.state.fetch_or(CLOSING_BIT, Ordering::AcqRel);
    }

    #[inline]
    pub(crate) fn acquire(&self) -> XllResult<()> {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |val| {
                if (val & CLOSING_BIT) != 0 {
                    None
                } else {
                    Some(val + 1)
                }
            })
            .map(|_| ())
            .map_err(|_| XllError::Closing)
    }

    #[inline]
    pub(crate) fn enter(&self) -> XllResult<OperationGuard<'_>> {
        self.acquire()?;
        Ok(OperationGuard { gate: self })
    }

    pub(crate) fn close_and_wait_begin(&self) -> TerminationWaitGuard<'_> {
        self.begin_close();
        TerminationWaitGuard { gate: self }
    }

    #[inline]
    pub(crate) fn leave(&self) {
        let prev = self.state.fetch_sub(1, Ordering::AcqRel);
        let active_count = (prev & !CLOSING_BIT) - 1;
        if active_count == 0 && (prev & CLOSING_BIT) != 0 {
            let _guard = self.wait_lock.lock();
            self.idle.notify_all();
        }
    }
}

pub(crate) struct OperationGuard<'a> {
    pub(crate) gate: &'a OperationGate,
}

impl Drop for OperationGuard<'_> {
    #[inline]
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
