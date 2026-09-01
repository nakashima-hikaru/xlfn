//! A generic admission gate for operations that must drain before shutdown.

#![allow(
    unsafe_code,
    reason = "owned operation guards are audited non-owning shutdown capabilities"
)]

use crate::drain_gate::{DrainGate, DrainPermit};
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GateClosed;

pub struct OperationGate {
    drain: DrainGate,
}

impl Default for OperationGate {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationGate {
    pub const fn new() -> Self {
        Self {
            drain: DrainGate::new_open(),
        }
    }

    #[inline]
    pub fn is_closing(&self) -> bool {
        self.drain.is_sealed()
    }

    #[inline]
    pub fn begin_close(&self) {
        self.drain.seal();
    }

    #[inline]
    pub fn acquire(&self) -> Result<(), GateClosed> {
        self.drain.try_acquire().map_err(|_| GateClosed)
    }

    #[inline]
    pub fn enter(&self) -> Result<OperationGuard<'_>, GateClosed> {
        let permit = self.drain.try_enter().map_err(|_| GateClosed)?;
        Ok(OperationGuard { _permit: permit })
    }

    /// Enters an operation whose guard cannot borrow the gate directly.
    ///
    /// # Safety
    ///
    /// The gate owner must not reclaim the gate until the returned guard has
    /// been dropped. Shutdown should seal and drain this same gate before
    /// reclaiming its owner.
    pub unsafe fn enter_owned(&self) -> Result<OwnedOperationGuard, GateClosed> {
        self.acquire()?;
        Ok(OwnedOperationGuard {
            gate: NonNull::from(self),
        })
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
        self.drain.release();
    }
}

pub struct OperationGuard<'a> {
    _permit: DrainPermit<'a>,
}

pub struct OwnedOperationGuard {
    gate: NonNull<OperationGate>,
}

impl Drop for OwnedOperationGuard {
    fn drop(&mut self) {
        // SAFETY: guaranteed by `OperationGate::enter_owned`; this guard is
        // itself the outstanding operation that delays owner reclamation.
        unsafe { self.gate.as_ref() }.release();
    }
}

// SAFETY: OperationGate is thread-safe and may release an operation from any
// thread; temporal validity is guaranteed by the constructor contract.
unsafe impl Send for OwnedOperationGuard {}
unsafe impl Sync for OwnedOperationGuard {}

pub struct TerminationWaitGuard<'a> {
    gate: &'a OperationGate,
}

impl TerminationWaitGuard<'_> {
    pub fn wait(self) {
        self.gate.drain.wait_until_idle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_acquisition_is_drained_by_the_close_wait() {
        let gate = OperationGate::new();
        gate.acquire().unwrap();
        let wait = gate.close_and_wait_begin();
        assert!(gate.acquire().is_err());
        gate.release();
        wait.wait();
    }

    #[test]
    fn an_operation_guard_releases_its_permit() {
        let gate = OperationGate::new();
        let operation = gate.enter().unwrap();
        gate.begin_close();
        drop(operation);
        gate.close_and_wait_begin().wait();
    }
}
