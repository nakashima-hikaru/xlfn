//! Host-independent contracts implemented by xlfn's lifecycle coordinator.
//! Numeric identities identify observations; they do not constitute authority
//! to publish, retire, or reclaim a generation. Such tickets are issued by the
//! coordinator that owns the corresponding resource.

pub mod diagnostic_id;
pub mod error;
pub mod generation;
pub mod lifecycle_sequence;
pub mod panic_boundary;
pub mod quarantine;
pub mod value_types;

/// Distinguishes a known host mutation from rejection or uncertainty.
pub enum HostMutation<T, Status, Error> {
    /// The mutation is known to have happened. Do not retry on cleanup failure.
    Applied {
        value: T,
        cleanup: Result<(), Error>,
    },
    /// The operation is known not to have happened.
    Rejected { error: Error },
    /// The returned status cannot establish whether a mutation happened.
    Indeterminate { status: Status, error: Error },
}

/// The generated host exports that request terminal logical removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeactivationExport {
    /// Excel's xlAutoClose is terminal even if xlAutoRemove was not called.
    AutoClose,
    /// Excel's explicit xlAutoRemove requests terminal logical removal.
    AutoRemove,
}

impl DeactivationExport {
    /// Diagnostic context for the common STA deactivation driver.
    #[must_use]
    pub const fn diagnostic_context(self) -> &'static str {
        match self {
            Self::AutoClose => "xlAutoClose lifecycle thread",
            Self::AutoRemove => "xlAutoRemove lifecycle thread",
        }
    }

    /// Only the close export can subsequently present a physical-release boundary.
    #[must_use]
    pub const fn permits_physical_release_boundary(self) -> bool {
        matches!(self, Self::AutoClose)
    }
}

#[cfg(test)]
mod tests {
    use super::DeactivationExport::*;

    #[test]
    fn remove_never_confers_the_physical_release_boundary() {
        assert!(AutoClose.permits_physical_release_boundary());
        assert!(!AutoRemove.permits_physical_release_boundary());
        assert_eq!(
            AutoClose.diagnostic_context(),
            "xlAutoClose lifecycle thread"
        );
        assert_eq!(
            AutoRemove.diagnostic_context(),
            "xlAutoRemove lifecycle thread"
        );
    }
}

/// Which lifecycle driver owns a stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopGoal {
    /// Compensate a failed open.
    RollbackOpen,
    /// Remove a live generation.
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopProgress {
    LogicalClosed,
    BindingReleased,
    ReturningSuccess,
}

/// Monotone boundary progress. The caller still owns the cleanup ticket and
/// must establish that its module epoch is current. This record cannot issue
/// a ticket or certify the underlying native operations.
pub struct StopRecord<Epoch> {
    attempt: generation::RemovalAttemptId,
    epoch: Epoch,
    goal: StopGoal,
    progress: StopProgress,
    quarantine_requested: bool,
}

impl<Epoch: Copy> StopRecord<Epoch> {
    /// Starts external-boundary finalization after logical close.
    pub const fn new(attempt: generation::RemovalAttemptId, epoch: Epoch, goal: StopGoal) -> Self {
        Self {
            attempt,
            epoch,
            goal,
            progress: StopProgress::LogicalClosed,
            quarantine_requested: false,
        }
    }

    pub const fn attempt(&self) -> generation::RemovalAttemptId {
        self.attempt
    }

    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub const fn goal(&self) -> StopGoal {
        self.goal
    }

    /// Revokes success without forgetting the currently outstanding owner.
    pub fn request_quarantine(&mut self) {
        self.quarantine_requested = true;
    }

    /// Whether an outstanding finalizer must join quarantine instead of committing success.
    pub const fn quarantine_requested(&self) -> bool {
        self.quarantine_requested
    }

    /// Records a completed release once, with exact ticket identity.
    pub fn binding_released(&mut self, attempt: generation::RemovalAttemptId) -> bool {
        if self.attempt != attempt || self.progress != StopProgress::LogicalClosed {
            return false;
        }
        // Record the already-completed native action even if quarantine won.
        // Returning success remains forbidden in that case.
        self.progress = StopProgress::BindingReleased;
        true
    }

    /// Records successful return only after the binding release has completed.
    pub fn returning_success(&mut self, attempt: generation::RemovalAttemptId) -> bool {
        self.advance(
            attempt,
            StopProgress::BindingReleased,
            StopProgress::ReturningSuccess,
        )
    }

    /// Checks whether the ticket is entitled to complete the boundary.
    pub fn may_complete(&self, attempt: generation::RemovalAttemptId) -> bool {
        self.attempt == attempt
            && !self.quarantine_requested
            && self.progress == StopProgress::ReturningSuccess
    }

    fn advance(
        &mut self,
        attempt: generation::RemovalAttemptId,
        from: StopProgress,
        to: StopProgress,
    ) -> bool {
        if self.attempt != attempt || self.quarantine_requested || self.progress != from {
            return false;
        }
        self.progress = to;
        true
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;

    #[test]
    fn stop_checks_ticket_order_and_quarantine_before_completion() {
        let id = generation::RemovalAttemptId::ONE;
        let other = id.checked_next().unwrap();
        let mut stop = StopRecord::new(id, 3u64, StopGoal::Remove);
        assert!(!stop.returning_success(id));
        assert!(!stop.binding_released(other));
        assert!(stop.binding_released(id));
        assert!(!stop.binding_released(id));
        assert!(stop.returning_success(id));
        assert!(stop.may_complete(id));
        assert!(!stop.may_complete(other));
        stop.request_quarantine();
        assert!(!stop.may_complete(id));
        let mut revoked = StopRecord::new(id, 3u64, StopGoal::Remove);
        revoked.request_quarantine();
        assert!(revoked.binding_released(id));
        assert!(!revoked.returning_success(id));
    }
}
