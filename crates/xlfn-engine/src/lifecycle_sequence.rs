//! Monotone identity decisions made under the canonical lifecycle lock.
//! Issuing an ID is not ownership: the caller must also install the selected
//! native payload and its recovery owner before releasing the lock.

use crate::generation::{OpenAttemptId, RemovalAttemptId, RemovalEpoch, RuntimeGeneration};

/// An open request that did not receive an attempt.
#[derive(Debug, Eq, PartialEq)]
pub enum OpenClaimError {
    /// Either a canonical owner is still active or removal invalidated the request.
    Unavailable,
    /// The monotonic attempt sequence cannot allocate another attempt.
    Exhausted,
    /// Zero is reserved and never names an actual open.
    InvalidSequence,
}

/// All monotonic decisions associated with starts and removals for one runtime.
pub struct LifecycleSequence {
    next_open: u64,
    next_removal: RemovalAttemptId,
    removal_epoch: RemovalEpoch,
}

impl LifecycleSequence {
    pub const fn new() -> Self {
        Self {
            next_open: 1,
            next_removal: RemovalAttemptId::ONE,
            removal_epoch: RemovalEpoch::new(0),
        }
    }

    /// Sampled by open callers before attempting to acquire the canonical lock.
    pub const fn removal_epoch(&self) -> RemovalEpoch {
        self.removal_epoch
    }

    /// Invalidate opens sampled before this removal request.
    pub fn request_removal(&mut self) -> Option<()> {
        self.removal_epoch = RemovalEpoch::new(self.removal_epoch.get().checked_add(1)?);
        Some(())
    }

    /// Allocate the identity after deciding the canonical state is idle Closed.
    /// A stale request or exhausted sequence leaves all identities unchanged.
    pub fn claim_open(
        &mut self,
        idle_closed: bool,
        sampled_epoch: RemovalEpoch,
    ) -> Result<OpenAttemptId, OpenClaimError> {
        if !idle_closed || sampled_epoch != self.removal_epoch {
            return Err(OpenClaimError::Unavailable);
        }
        let next = self
            .next_open
            .checked_add(1)
            .ok_or(OpenClaimError::Exhausted)?;
        let attempt = OpenAttemptId::new(self.next_open).ok_or(OpenClaimError::InvalidSequence)?;
        self.next_open = next;
        Ok(attempt)
    }

    /// Allocate a removal identity only after selecting its canonical owner.
    pub fn claim_removal(&mut self) -> Option<RemovalAttemptId> {
        let attempt = self.next_removal;
        self.next_removal = attempt.checked_next()?;
        Some(attempt)
    }

    /// Whether the supplied generation corresponds to the latest issued open.
    pub fn is_latest_open(&self, generation: RuntimeGeneration) -> bool {
        generation.get().checked_add(1) == Some(self.next_open)
    }

    /// Publish for test/support callers needing to check sequence exhaustion
    /// without issuing a prohibitively large number of attempts.
    #[doc(hidden)]
    pub const fn seeded_next_open_for_test(mut self, next: u64) -> Self {
        self.next_open = next;
        self
    }

    /// Read the next raw value for tests that check issuance is all-or-nothing.
    #[doc(hidden)]
    pub const fn next_open_for_test(&self) -> u64 {
        self.next_open
    }
}

impl Default for LifecycleSequence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_and_non_idle_open_requests_leave_attempt_unchanged() {
        let mut sequence = LifecycleSequence::new();
        let stale = sequence.removal_epoch();
        sequence.request_removal().unwrap();
        let current = sequence.removal_epoch();
        assert_eq!(
            sequence.claim_open(true, stale),
            Err(OpenClaimError::Unavailable)
        );
        assert_eq!(
            sequence.claim_open(false, current),
            Err(OpenClaimError::Unavailable)
        );
        let open = sequence.claim_open(true, current).unwrap();
        assert_eq!(open.get(), 1);
        assert!(sequence.is_latest_open(open.into_runtime_generation()));
        assert_eq!(sequence.claim_removal(), Some(RemovalAttemptId::ONE));
        assert_eq!(sequence.claim_open(true, current).unwrap().get(), 2);
        assert!(!sequence.is_latest_open(open.into_runtime_generation()));
    }

    #[test]
    fn exhaustion_and_reserved_zero_do_not_mutate_sequence() {
        let mut exhausted = LifecycleSequence::new().seeded_next_open_for_test(u64::MAX);
        assert_eq!(
            exhausted.claim_open(true, exhausted.removal_epoch()),
            Err(OpenClaimError::Exhausted)
        );
        assert_eq!(exhausted.next_open_for_test(), u64::MAX);
        let mut zero = LifecycleSequence::new().seeded_next_open_for_test(0);
        assert_eq!(
            zero.claim_open(true, zero.removal_epoch()),
            Err(OpenClaimError::InvalidSequence)
        );
        assert_eq!(zero.next_open_for_test(), 0);
    }
}
