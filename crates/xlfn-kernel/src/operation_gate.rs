//! A generic admission gate for operations that must drain before shutdown.

use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GateClosed;

pub struct OperationGate {
    state: AtomicUsize,
    wait_lock: Mutex<()>,
    idle: Condvar,
}

pub const CLOSING_BIT: usize = usize::MAX / 2 + 1;
const ACTIVE_COUNT_MASK: usize = !CLOSING_BIT;

impl Default for OperationGate {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationGate {
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    #[inline]
    pub fn is_closing(&self) -> bool {
        (self.state.load(Ordering::Acquire) & CLOSING_BIT) != 0
    }

    #[inline]
    pub fn begin_close(&self) {
        self.state.fetch_or(CLOSING_BIT, Ordering::AcqRel);
    }

    #[inline]
    pub fn acquire(&self) -> Result<(), GateClosed> {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |val| {
                if (val & CLOSING_BIT) != 0 {
                    None
                } else {
                    if (val & ACTIVE_COUNT_MASK) == ACTIVE_COUNT_MASK {
                        std::process::abort();
                    }
                    Some(val + 1)
                }
            })
            .map(|_| ())
            .map_err(|_| GateClosed)
    }

    #[inline]
    pub fn enter(&self) -> Result<OperationGuard<'_>, GateClosed> {
        self.acquire()?;
        Ok(OperationGuard { gate: self })
    }

    pub fn close_and_wait_begin(&self) -> TerminationWaitGuard<'_> {
        self.begin_close();
        TerminationWaitGuard { gate: self }
    }

    /// Releases a count acquired with [`OperationGate::acquire`].
    ///
    /// This is used when the acquired operation owns an `Arc` rather than a
    /// borrow of the gate and therefore cannot store [`OperationGuard`].
    #[inline]
    pub fn release(&self) {
        let prev = self.state.fetch_sub(1, Ordering::AcqRel);
        let active = prev & ACTIVE_COUNT_MASK;
        if active == 0 {
            std::process::abort();
        }
        let active_count = active - 1;
        if active_count == 0 && (prev & CLOSING_BIT) != 0 {
            let _guard = self.wait_lock.lock();
            self.idle.notify_all();
        }
    }
}

pub struct OperationGuard<'a> {
    gate: &'a OperationGate,
}

impl Drop for OperationGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.gate.release();
    }
}

pub struct TerminationWaitGuard<'a> {
    gate: &'a OperationGate,
}

impl TerminationWaitGuard<'_> {
    pub fn wait(self) {
        let mut guard = self.gate.wait_lock.lock();
        while (self.gate.state.load(Ordering::Acquire) & ACTIVE_COUNT_MASK) > 0 {
            self.gate.idle.wait(&mut guard);
        }
    }
}
