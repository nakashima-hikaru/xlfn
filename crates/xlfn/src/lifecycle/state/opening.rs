//! The locked, issuer-bound start of one opening transaction.

use super::{ClosedState, LifecycleAccess, LifecycleCoordinator, LifecycleState};
use crate::generation::{OpenAttemptId, RemovalEpoch};
use crate::{Addin, XllError, XllResult};

/// Owns the canonical opening attempt and its original lock until the runtime
/// installs its existing opening owner. Abandonment retains terminal state;
/// forgetting this owner retains the lock rather than enabling another start.
#[must_use = "transfer this start into the opening transaction"]
pub(crate) struct OpenStart<'coordinator, A: Addin> {
    control: Option<LifecycleAccess<'coordinator, A>>,
    attempt: OpenAttemptId,
}

impl<'coordinator, A: Addin> OpenStart<'coordinator, A> {
    pub(in crate::lifecycle) fn acquire(
        coordinator: &'coordinator LifecycleCoordinator<A>,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<Self> {
        let mut control = coordinator.access();
        if control.removal_epoch() != expected_removal_epoch.get()
            || !matches!(
                control.canonical_state(),
                LifecycleState::Closed(ClosedState::Idle)
            )
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_PHASE,
            });
        }
        // Exhaustion must leave host intent and the closed state unchanged.
        let attempt = coordinator.allocate_open_attempt(&mut control)?;
        coordinator.prepare_open(&mut control);
        coordinator.begin_opening(&mut control, attempt);
        Ok(Self {
            control: Some(control),
            attempt,
        })
    }

    pub(crate) fn attempt(&self) -> OpenAttemptId {
        self.attempt
    }

    pub(crate) fn access(&self) -> &LifecycleAccess<'coordinator, A> {
        self.control.as_ref().expect("open start retains its lock")
    }

    pub(crate) fn return_module(&mut self, closing: crate::module_runtime::ModuleClosing) {
        let control = self.control.as_mut().expect("open start retains its lock");
        control
            .coordinator
            .complete_open_abort_locked(control, closing);
    }

    /// The runtime's narrow acquisition bridge first installs `Begun`, then
    /// consumes this recovery responsibility and unlocks before callbacks.
    pub(crate) fn transfer_to_opening(mut self) {
        self.control.take();
    }
}

impl<A: Addin> Drop for OpenStart<'_, A> {
    fn drop(&mut self) {
        if let Some(control) = self.control.as_mut() {
            control.coordinator.quarantine_core(control);
            control.coordinator.notify_all();
        }
    }
}

#[cfg(test)]
mod tests;
