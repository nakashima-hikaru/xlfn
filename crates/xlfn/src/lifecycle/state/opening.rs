//! The locked, issuer-bound start of one opening transaction.

use super::{LifecycleAccess, LifecycleCoordinator};
use crate::generation::{OpenAttemptId, RemovalEpoch};
use crate::{Addin, XllResult};
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

/// Owns the canonical opening attempt and its original lock until the runtime
/// installs its existing opening owner. Abandonment retains terminal state;
/// forgetting this owner retains the lock rather than enabling another start.
#[must_use = "transfer this start into the opening transaction"]
pub(crate) struct OpenStart<'coordinator, A: Addin> {
    control: RollbackGuard<OpenStartLock<'coordinator, A>>,
    attempt: OpenAttemptId,
}

struct OpenStartLock<'coordinator, A: Addin> {
    access: LifecycleAccess<'coordinator, A>,
}

impl<A: Addin> Rollback for OpenStartLock<'_, A> {
    fn rollback(self) {
        let mut control = self.access;
        // Resolve the opening while this owner still holds the canonical lock.
        control.coordinator.record_open_failure(&mut control);
        control.coordinator.quarantine_core(&mut control);
        control.coordinator.notify_all();
    }
}

/// A rejected handoff retains both recovery owners. Field order is intentional:
/// release the canonical start lock before the successor's rollback can reenter
/// its coordinator. Neither owner can be extracted by ordinary orchestration.
pub(crate) struct OpeningHandoffFailure<'coordinator, A: Addin> {
    _start: OpenStart<'coordinator, A>,
    _successor: crate::runtime::OpeningTxn<'coordinator, A, crate::runtime::Begun<A>>,
}

impl<'coordinator, A: Addin> OpenStart<'coordinator, A> {
    pub(in crate::lifecycle) fn acquire(
        coordinator: &'coordinator LifecycleCoordinator<A>,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<Self> {
        let mut control = coordinator.access();
        // Exhaustion must leave host intent and the closed state unchanged.
        let attempt = coordinator.begin_opening_if_closed(&mut control, expected_removal_epoch)?;
        Ok(Self {
            control: RollbackGuard::new(OpenStartLock { access: control }),
            attempt,
        })
    }

    pub(crate) fn attempt(&self) -> OpenAttemptId {
        self.attempt
    }

    pub(crate) fn access(&self) -> &LifecycleAccess<'coordinator, A> {
        &self.control.get().access
    }

    pub(crate) fn return_module(
        &mut self,
        closing: crate::module_runtime::ModuleClosing,
    ) -> Result<(), crate::module_runtime::ModuleAuthority> {
        let control = &mut self.control.get_mut().access;
        control
            .coordinator
            .complete_open_abort_locked(control, closing)
    }

    /// Transfer recovery only to the concrete successor for this issuer and
    /// attempt. Its constructor is private to the runtime acquisition bridge;
    /// no unit-returning disarm or caller-supplied boolean can release the lock.
    #[allow(
        clippy::result_large_err,
        reason = "a failed handoff retains both affine recovery owners without allocating"
    )]
    pub(crate) fn transfer_to_opening(
        self,
        successor: crate::runtime::OpeningTxn<'coordinator, A, crate::runtime::Begun<A>>,
    ) -> Result<
        crate::runtime::OpeningTxn<'coordinator, A, crate::runtime::Begun<A>>,
        OpeningHandoffFailure<'coordinator, A>,
    > {
        if !successor.matches_start(self.access().coordinator, self.attempt) {
            return Err(OpeningHandoffFailure {
                _start: self,
                _successor: successor,
            });
        }
        drop(self.control.complete());
        Ok(successor)
    }
}

#[cfg(test)]
mod tests;
