//! A fail-stop active counter that can be sealed and reopened after draining.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::invariant::fail_stop;

/// The counter has been sealed and no new permit may be acquired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sealed;

/// Reopening is only valid for a sealed, idle counter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReopenError;

/// The result of releasing one active permit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseOutcome {
    StillActive,
    BecameIdle,
}

/// A bounded active counter with sealed and drain-notification bits.
///
/// This type deliberately knows nothing about lifecycle phases, generations,
/// or the resource represented by a permit.  It only provides the mechanical
/// acquire/seal/release/reopen protocol used by higher-level domains.
pub struct SealableCounter {
    state: AtomicUsize,
}

pub const SEALED_BIT: usize = 1_usize << (usize::BITS - 1);
const WAITING_BIT: usize = SEALED_BIT >> 1;
const ACTIVE_COUNT_MASK: usize = WAITING_BIT - 1;

#[inline]
fn acquire_update(state: usize) -> Option<usize> {
    if state & SEALED_BIT != 0 {
        return None;
    }
    if state & ACTIVE_COUNT_MASK == ACTIVE_COUNT_MASK {
        fail_stop();
    }
    Some(state + 1)
}

#[inline]
fn release_update(state: usize) -> Option<usize> {
    (state & ACTIVE_COUNT_MASK != 0).then(|| state - 1)
}

#[inline]
fn release_without_notification_update(state: usize) -> Option<usize> {
    let active = state & ACTIVE_COUNT_MASK;
    if active == 0 {
        fail_stop();
    }
    // Keep the last count live until the drain gate owns its notification
    // mutex. This also keeps the gate alive while that mutex is acquired.
    if active == 1 && state & WAITING_BIT != 0 {
        return None;
    }
    Some(state - 1)
}

#[inline]
fn release_outcome(previous: usize) -> ReleaseOutcome {
    if previous & ACTIVE_COUNT_MASK == 1 {
        ReleaseOutcome::BecameIdle
    } else {
        ReleaseOutcome::StillActive
    }
}

#[inline]
fn reopen_update(state: usize) -> Option<usize> {
    (state & SEALED_BIT != 0 && state & ACTIVE_COUNT_MASK == 0).then_some(0)
}

impl SealableCounter {
    pub const fn new_open() -> Self {
        Self {
            state: AtomicUsize::new(0),
        }
    }

    pub const fn new_sealed() -> Self {
        Self {
            state: AtomicUsize::new(SEALED_BIT),
        }
    }

    #[inline]
    pub fn try_acquire(&self) -> Result<(), Sealed> {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, acquire_update)
            .map(|_| ())
            .map_err(|_| Sealed)
    }

    #[inline]
    pub fn release(&self) -> ReleaseOutcome {
        let previous = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, release_update)
            .unwrap_or_else(|_| fail_stop());
        release_outcome(previous)
    }

    /// Attempts a release that will need no further access to the drain gate.
    /// `None` retains the last count until the gate acquires its wait mutex.
    #[inline]
    pub(crate) fn try_release_without_notification(&self) -> Option<ReleaseOutcome> {
        self.state
            .try_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                release_without_notification_update,
            )
            .ok()
            .map(release_outcome)
    }

    /// Registers drain notification in the same atomic state as admission and
    /// release, and returns the active count observed by that RMW. The bit is
    /// sticky until a serialized reopen; no independent waiter counter exists.
    pub(crate) fn mark_waiting(&self) -> usize {
        self.state.fetch_or(WAITING_BIT, Ordering::AcqRel) & ACTIVE_COUNT_MASK
    }

    #[inline]
    pub fn seal(&self) {
        self.state.fetch_or(SEALED_BIT, Ordering::AcqRel);
    }

    /// Seals an open, idle counter in one atomic operation. A failed attempt
    /// leaves the counter unchanged, including when a reader won admission.
    #[inline]
    pub(crate) fn try_seal_if_idle(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        if state & (SEALED_BIT | ACTIVE_COUNT_MASK) != 0 {
            return false;
        }
        self.state
            .compare_exchange(
                state,
                state | SEALED_BIT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Rolls back an idle seal without discarding an existing notification
    /// obligation. Unlike a new generation's reopen, rollback may race a wait.
    pub(crate) fn undo_idle_seal(&self) -> Result<(), ReopenError> {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & SEALED_BIT != 0 && state & ACTIVE_COUNT_MASK == 0)
                    .then_some(state & !SEALED_BIT)
            })
            .map(|_| ())
            .map_err(|_| ReopenError)
    }

    /// Reopens a sealed, idle counter and clears its notification obligation.
    /// Drain gates serialize this reset with waiter registration.
    pub fn reopen(&self) -> Result<(), ReopenError> {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, reopen_update)
            .map(|_| ())
            .map_err(|_| ReopenError)
    }

    #[inline]
    pub fn is_sealed(&self) -> bool {
        self.state.load(Ordering::Acquire) & SEALED_BIT != 0
    }

    #[inline]
    pub fn active(&self) -> usize {
        self.state.load(Ordering::Acquire) & ACTIVE_COUNT_MASK
    }
}

impl std::fmt::Debug for SealableCounter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealableCounter")
            .field("sealed", &self.is_sealed())
            .field("active", &self.active())
            .finish()
    }
}

/// Loom's atomic backend uses the production state transitions and orderings.
/// Keeping this adapter next to those transitions prevents the model from
/// silently validating a different notification or count protocol.
#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86"))))]
pub(crate) mod loom_support {
    use super::*;
    use loom::sync::atomic::AtomicUsize;

    pub(crate) struct Counter {
        state: AtomicUsize,
    }

    impl Counter {
        pub(crate) fn new() -> Self {
            Self {
                state: AtomicUsize::new(0),
            }
        }

        pub(crate) fn try_acquire(&self) -> Result<(), Sealed> {
            self.state
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, acquire_update)
                .map(|_| ())
                .map_err(|_| Sealed)
        }

        pub(crate) fn try_release_without_notification(&self) -> Option<ReleaseOutcome> {
            self.state
                .fetch_update(
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    release_without_notification_update,
                )
                .ok()
                .map(release_outcome)
        }

        pub(crate) fn release(&self) -> ReleaseOutcome {
            let previous = self
                .state
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, release_update)
                .expect("model releases an admitted count");
            release_outcome(previous)
        }

        pub(crate) fn mark_waiting(&self) -> usize {
            self.state.fetch_or(WAITING_BIT, Ordering::AcqRel) & ACTIVE_COUNT_MASK
        }

        pub(crate) fn seal(&self) {
            self.state.fetch_or(SEALED_BIT, Ordering::AcqRel);
        }

        pub(crate) fn reopen(&self) -> Result<(), ReopenError> {
            self.state
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, reopen_update)
                .map(|_| ())
                .map_err(|_| ReopenError)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_rejects_new_acquisitions_until_reopened() {
        let counter = SealableCounter::new_open();
        counter.try_acquire().unwrap();
        assert_eq!(counter.active(), 1);
        counter.seal();
        assert_eq!(counter.try_acquire(), Err(Sealed));
        assert_eq!(counter.release(), ReleaseOutcome::BecameIdle);
        assert!(counter.reopen().is_ok());
        assert!(!counter.is_sealed());
    }

    #[test]
    fn reopen_requires_a_sealed_idle_counter() {
        let counter = SealableCounter::new_open();
        assert_eq!(counter.reopen(), Err(ReopenError));
        counter.seal();
        counter.try_acquire().unwrap_err();
        assert!(counter.reopen().is_ok());
    }

    #[test]
    fn miri_notification_registration_retains_the_last_count() {
        let counter = SealableCounter::new_open();
        counter.try_acquire().unwrap();
        counter.try_acquire().unwrap();
        assert_eq!(counter.mark_waiting(), 2);
        assert_eq!(counter.active(), 2, "notification bit is not a permit");
        assert_eq!(
            counter.try_release_without_notification(),
            Some(ReleaseOutcome::StillActive)
        );
        assert_eq!(counter.try_release_without_notification(), None);
        assert_eq!(counter.active(), 1, "slow release must keep its owner live");
        assert_eq!(counter.release(), ReleaseOutcome::BecameIdle);

        counter.seal();
        counter.reopen().unwrap();
        counter.try_acquire().unwrap();
        assert_eq!(
            counter.try_release_without_notification(),
            Some(ReleaseOutcome::BecameIdle),
            "a new generation starts without a notification obligation"
        );
    }

    #[test]
    fn idle_seal_rollback_preserves_notification_registration() {
        let counter = SealableCounter::new_open();
        assert_eq!(counter.mark_waiting(), 0);
        assert!(counter.try_seal_if_idle());
        counter.undo_idle_seal().unwrap();
        counter.try_acquire().unwrap();
        assert_eq!(counter.try_release_without_notification(), None);
        assert_eq!(counter.release(), ReleaseOutcome::BecameIdle);
    }
}
