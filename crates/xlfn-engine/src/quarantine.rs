//! Single-writer arbitration for terminal module cleanup.
//! Host payloads never enter this protocol. The caller must claim, extract,
//! and later reinstall its opaque owner under its own coordinator lock.

mod transitions;
use std::num::NonZeroU64;
use transitions::{claim_cleanup, complete_cleanup};

/// An affine authorization for one cleanup incarnation, not a native owner.
#[must_use]
pub struct CleanupClaim<Issuer, Epoch> {
    issuer: Issuer,
    epoch: Epoch,
    incarnation: NonZeroU64,
}

impl<Issuer, Epoch: Copy> CleanupClaim<Issuer, Epoch> {
    /// Epoch of the resource the issuer authorized the driver to clean.
    pub const fn epoch(&self) -> Epoch {
        self.epoch
    }
}

/// How to handle an already-started cleanup before consulting host ownership.
pub enum CleanupObservation<Epoch> {
    /// An owner can be selected from the canonical payload.
    SelectOwner,
    /// The same epoch's running claim already owns cleanup.
    Running,
    /// The same epoch finished draining.
    Drained,
    /// The same epoch could not drain and must remain loaded.
    Retained(Epoch),
    /// A previous epoch must not affect the current one.
    Stale,
}

/// Outcome published only by an exactly matching running claim.
pub enum CleanupOutcome {
    Drained,
    Retained,
}

enum Progress<Issuer, Epoch> {
    Idle,
    Pending,
    Running {
        issuer: Issuer,
        epoch: Epoch,
        incarnation: NonZeroU64,
    },
    Drained(Epoch),
    Retained(Epoch),
}

/// Canonical arbitration state; only its immutable observation is exposed.
/// This is not a second lifecycle phase: it arbitrates ownership of the
/// asynchronous cleanup operation within the caller's terminal lifecycle.
pub struct CleanupProtocol<Issuer, Epoch> {
    progress: Progress<Issuer, Epoch>,
    next_incarnation: NonZeroU64,
}

impl<Issuer: Copy + Eq, Epoch: Copy + Eq> CleanupProtocol<Issuer, Epoch> {
    pub const fn new() -> Self {
        Self {
            progress: Progress::Idle,
            next_incarnation: NonZeroU64::MIN,
        }
    }

    pub fn observe(&self, current: impl Fn(Epoch) -> bool) -> CleanupObservation<Epoch> {
        match self.progress {
            Progress::Idle | Progress::Pending => CleanupObservation::SelectOwner,
            Progress::Running { epoch, .. } if current(epoch) => CleanupObservation::Running,
            Progress::Drained(epoch) if current(epoch) => CleanupObservation::Drained,
            Progress::Retained(epoch) if current(epoch) => CleanupObservation::Retained(epoch),
            _ => CleanupObservation::Stale,
        }
    }

    pub fn defer(&mut self) {
        if matches!(self.progress, Progress::Idle) {
            self.progress = Progress::Pending;
        }
    }

    /// Call only while the issuer holds its canonical lock and can transfer
    /// the unique native owner into the returned ticket's driver.
    pub fn claim(&mut self, issuer: Issuer, epoch: Epoch) -> Option<CleanupClaim<Issuer, Epoch>> {
        claim_cleanup!(self.progress, self.next_incarnation, issuer, epoch)
    }

    /// Atomically verifies issuer, incarnation and current epoch before changing
    /// progress. A rejected completion leaves all decisions untouched.
    pub fn complete(
        &mut self,
        issuer: Issuer,
        claim: &CleanupClaim<Issuer, Epoch>,
        current: impl FnOnce(Epoch) -> bool,
        outcome: CleanupOutcome,
    ) -> bool {
        complete_cleanup!(self.progress, issuer, claim, epoch => current(epoch), outcome)
    }
}

impl<Issuer: Copy + Eq, Epoch: Copy + Eq> Default for CleanupProtocol<Issuer, Epoch> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{CleanupObservation as Obs, CleanupOutcome as Outcome, CleanupProtocol};

    #[test]
    fn pending_owner_transfers_once_and_completion_authenticates_issuer_and_epoch() {
        let mut cleanup = CleanupProtocol::new();
        let mut other = CleanupProtocol::new();
        cleanup.defer();
        assert!(matches!(cleanup.observe(|_| true), Obs::SelectOwner));
        let claim = cleanup.claim(1, 7).unwrap();
        let foreign = other.claim(2, 7).unwrap();
        assert!(cleanup.claim(1, 7).is_none());
        cleanup.defer();
        assert!(matches!(cleanup.observe(|epoch| epoch == 7), Obs::Running));
        assert!(matches!(cleanup.observe(|epoch| epoch == 8), Obs::Stale));
        assert!(!cleanup.complete(1, &foreign, |_| true, Outcome::Drained));
        assert!(!cleanup.complete(2, &claim, |_| true, Outcome::Drained));
        assert!(!cleanup.complete(1, &claim, |epoch| epoch == 8, Outcome::Drained));
        assert!(cleanup.complete(1, &claim, |epoch| epoch == 7, Outcome::Retained));
        assert!(matches!(cleanup.observe(|_| true), Obs::Retained(7)));
        assert!(!cleanup.complete(1, &claim, |_| true, Outcome::Drained));
        assert!(cleanup.claim(1, 8).is_none());
    }

    #[test]
    fn exhaustion_leaves_progress_and_incarnation_unchanged() {
        let mut cleanup = CleanupProtocol {
            progress: super::Progress::Pending,
            next_incarnation: super::NonZeroU64::MAX,
        };
        assert!(cleanup.claim(1, 7).is_none());
        assert_eq!(cleanup.next_incarnation, super::NonZeroU64::MAX);
        assert!(matches!(cleanup.observe(|_| true), Obs::SelectOwner));
    }
}
