//! A generic admission gate for operations that must drain before shutdown.

use crate::drain_gate::{DrainGate, DrainPermit};

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
