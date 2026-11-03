//! A generic admission gate for operations that must drain before shutdown.

#![allow(
    unsafe_code,
    reason = "owned operation guards are audited non-owning shutdown capabilities"
)]

use crate::drain_gate::{DetachedDrainPermit, DrainPermit, TerminalDrainGate};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GateClosed;

pub struct OperationGate {
    drain: TerminalDrainGate,
}

impl Default for OperationGate {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationGate {
    pub const fn new() -> Self {
        Self {
            drain: TerminalDrainGate::new(),
        }
    }

    #[inline]
    pub fn is_closing(&self) -> bool {
        self.drain.is_sealed()
    }

    #[inline]
    pub fn active(&self) -> usize {
        self.drain.active()
    }

    /// Begins closing the gate to prevent new admissions.
    ///
    /// Formal theorem [OG-1]: After this linearization point (`drain.seal()`),
    /// all future `enter` and `enter_owned` attempts will return `Err(GateClosed)`.
    #[inline]
    pub fn begin_close(&self) {
        self.drain.seal();
    }

    /// Enters an operation producing an RAII-scoped [`OperationGuard`].
    ///
    /// Formal theorem [OG-1], [OG-3]: Guarantees a live permit backed by active count.
    /// Linearization point: `self.drain.try_enter()`.
    #[inline]
    pub fn enter(&self) -> Result<OperationGuard<'_>, GateClosed> {
        let permit = self.drain.try_enter().map_err(|_| GateClosed)?;
        Ok(OperationGuard { _permit: permit })
    }

    /// Enters an operation whose guard cannot borrow the gate directly.
    ///
    /// # Safety
    ///
    /// The owner must retain the gate through all accesses by the returned
    /// guard, including its final release notification. Sealing and draining
    /// this same gate synchronizes with that final access; the owner may then
    /// be reclaimed if no other capability can access it.
    ///
    /// Formal theorem [OG-4]: Owned guard establishes a reclamation barrier until release.
    pub unsafe fn enter_owned(&self) -> Result<OwnedOperationGuard, GateClosed> {
        // SAFETY: the caller retains this gate through final release and drain.
        let permit = unsafe { self.drain.try_enter_detached() }.map_err(|_| GateClosed)?;
        Ok(OwnedOperationGuard { _permit: permit })
    }

    /// Seals the gate and prepares to wait for all active operations to quiesce.
    ///
    /// Formal theorem [OG-1], [OG-2]: Marks gate closed and initiates drain barrier.
    pub fn close_and_wait_begin(&self) -> TerminationWaitGuard<'_> {
        self.begin_close();
        TerminationWaitGuard { gate: self }
    }
}

pub struct OperationGuard<'a> {
    _permit: DrainPermit<'a>,
}

pub struct OwnedOperationGuard {
    _permit: DetachedDrainPermit,
}

impl OwnedOperationGuard {
    /// Explicit consuming counterpart of Drop. The exact gate is retained in
    /// the permit; callers cannot release an unrelated operation count.
    pub fn release(self) {
        drop(self);
    }
}

impl OperationGuard<'_> {
    pub fn release(self) {
        drop(self);
    }
}

#[must_use = "call wait to observe terminal operation drain"]
pub struct TerminationWaitGuard<'a> {
    gate: &'a OperationGate,
}

impl<'a> TerminationWaitGuard<'a> {
    /// Waits until all operations admitted before or during close have finished.
    ///
    /// Formal theorem [OG-2]: Upon return, `active == 0` and quiescence is certified.
    pub fn wait(self) -> OperationsDrained<'a> {
        self.gate.drain.wait();
        OperationsDrained { gate: self.gate }
    }
}

/// A persistent terminal observation, not exclusive ownership of any protected
/// allocation. OperationGate never reopens; this fact may be copied/reobserved.
#[derive(Clone, Copy)]
pub struct OperationsDrained<'a> {
    gate: &'a OperationGate,
}

impl OperationsDrained<'_> {
    pub fn belongs_to(&self, gate: &OperationGate) -> bool {
        std::ptr::eq(self.gate, gate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_permit_release_is_drained_by_the_close_wait() {
        let gate = OperationGate::new();
        let permit = gate.enter().unwrap();
        let wait = gate.close_and_wait_begin();
        assert!(gate.enter().is_err());
        permit.release();
        wait.wait();
    }

    #[test]
    fn miri_terminal_drain_observation_is_repeatable_and_bound_to_its_gate() {
        let gate = OperationGate::new();
        let other = OperationGate::new();
        let drained = gate.close_and_wait_begin().wait();
        let repeated = gate.close_and_wait_begin().wait();
        assert!(drained.belongs_to(&gate));
        assert!(repeated.belongs_to(&gate));
        assert!(!drained.belongs_to(&other));
        assert!(gate.enter().is_err());
    }

    #[test]
    fn an_operation_guard_releases_its_permit() {
        let gate = OperationGate::new();
        let operation = gate.enter().unwrap();
        gate.begin_close();
        drop(operation);
        gate.close_and_wait_begin().wait();
    }

    #[test]
    fn miri_owned_operation_drains_before_reclaiming_its_gate() {
        for _ in 0..8 {
            let owner = crate::published_owner::PublishedOwner::new(OperationGate::new());
            // SAFETY: sealing and waiting retains the owner through the
            // permit's final gate access, including its notification tail.
            let permit = unsafe { owner.enter_owned() }.unwrap();
            let worker = std::thread::spawn(move || drop(permit));
            owner.close_and_wait_begin().wait();
            drop(owner);
            worker.join().unwrap();
        }
    }
}
