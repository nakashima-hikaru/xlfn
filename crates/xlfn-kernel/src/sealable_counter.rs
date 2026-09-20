//! A fail-stop active counter that can be sealed and reopened after draining.

pub mod transitions;

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::invariant::fail_stop;

/// The counter has been sealed and no new permit may be acquired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sealed;

/// Reopening is only valid for a sealed, idle counter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReopenError;

pub use transitions::ReleaseOutcome;

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

#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(SEALED_BIT == transitions::SEALED_BIT_32 as usize);
    assert!(WAITING_BIT == transitions::WAITING_BIT_32 as usize);
    assert!(ACTIVE_COUNT_MASK == transitions::ACTIVE_COUNT_MASK_32 as usize);
};

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(SEALED_BIT == transitions::SEALED_BIT_64 as usize);
    assert!(WAITING_BIT == transitions::WAITING_BIT_64 as usize);
    assert!(ACTIVE_COUNT_MASK == transitions::ACTIVE_COUNT_MASK_64 as usize);
};

/// [SC-1] Active count is strictly bounded: active <= ACTIVE_COUNT_MASK.
/// [SC-2] Successful acquire: active' = active + 1, flags preserved.
/// [SC-3] Sealed rejects acquire: sealed => None.
#[inline]
fn acquire_update(state: usize) -> Option<usize> {
    match transitions::acquire_step(state) {
        transitions::TransitionOutcome::Success(next) => Some(next),
        transitions::TransitionOutcome::Rejected => None,
        transitions::TransitionOutcome::FailStop => fail_stop(),
    }
}

/// [SC-4] Release requires active > 0.
/// [SC-5] Successful release: active' = active - 1, flags preserved.
#[inline]
fn release_update(state: usize) -> Option<usize> {
    match transitions::release_step(state) {
        transitions::TransitionOutcome::Success(next) => Some(next),
        transitions::TransitionOutcome::Rejected => None,
        transitions::TransitionOutcome::FailStop => fail_stop(),
    }
}

/// [SC-9] Retain final capability: waiting && active == 1 => returns None.
/// Keeps the last count live until the drain gate owns its notification
/// mutex, preventing premature gate destruction.
/// [SC-9b] Successful release decrements active by 1 and preserves flags.
#[inline]
fn release_without_notification_update(state: usize) -> Option<usize> {
    match transitions::release_without_notification_step(state) {
        transitions::TransitionOutcome::Success(next) => Some(next),
        transitions::TransitionOutcome::Rejected => None,
        transitions::TransitionOutcome::FailStop => fail_stop(),
    }
}

/// [SC-6] BecameIdle equivalence: outcome is BecameIdle <=> previous active == 1.
#[inline]
fn release_outcome(previous: usize) -> ReleaseOutcome {
    transitions::release_outcome_step(previous)
}

/// [SC-7] Reopen precondition: requires sealed && active == 0.
/// [SC-8] Reopen postcondition: establishes !sealed && !waiting && active == 0.
#[inline]
fn reopen_update(state: usize) -> Option<usize> {
    match transitions::reopen_step(state) {
        transitions::TransitionOutcome::Success(next) => Some(next),
        transitions::TransitionOutcome::Rejected => None,
        transitions::TransitionOutcome::FailStop => fail_stop(),
    }
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

    /// Linearization Point: RMW update with `acquire_update`.
    /// Guaranteed by Verus: [SC-2] increments active by 1; [SC-3] rejects if sealed.
    #[inline]
    pub fn try_acquire(&self) -> Result<(), Sealed> {
        // Acquire a reopen's publication even if a delayed reader selected
        // this gate in an older generation. The count itself publishes no data.
        self.state
            .try_update(Ordering::Acquire, Ordering::Relaxed, acquire_update)
            .map(|_| ())
            .map_err(|_| Sealed)
    }

    /// Linearization Point: RMW update with `release_update`.
    /// Guaranteed by Verus: [SC-4] requires active > 0; [SC-5] decrements active by 1; [SC-6] BecameIdle iff active == 1.
    #[inline]
    pub fn release(&self) -> ReleaseOutcome {
        // Publish protected accesses to an acquiring idle observer. Every
        // state update is an RMW, so intermediate admissions/releases preserve
        // the release sequence without acquiring earlier holders' accesses.
        let previous = self
            .state
            .try_update(Ordering::Release, Ordering::Relaxed, release_update)
            .unwrap_or_else(|_| fail_stop());
        release_outcome(previous)
    }

    /// Attempts a release that will need no further access to the drain gate.
    /// `None` retains the last count until the gate acquires its wait mutex.
    ///
    /// Linearization Point: RMW update with `release_without_notification_update`.
    /// Guaranteed by Verus: [SC-9] safely refuses decrement when waiting && active == 1.
    #[inline]
    pub(crate) fn try_release_without_notification(&self) -> Option<ReleaseOutcome> {
        self.state
            .try_update(
                Ordering::Release,
                Ordering::Relaxed,
                release_without_notification_update,
            )
            .ok()
            .map(release_outcome)
    }

    /// Registers drain notification in the same atomic state as admission and
    /// release, and returns the active count observed by that RMW. The bit is
    /// sticky until a serialized reopen; no independent waiter counter exists.
    ///
    /// Linearization Point: atomic fetch_or setting WAITING_BIT.
    pub(crate) fn mark_waiting(&self) -> usize {
        self.state.fetch_or(WAITING_BIT, Ordering::AcqRel) & ACTIVE_COUNT_MASK
    }

    /// Linearization Point: atomic fetch_or setting SEALED_BIT.
    /// Guaranteed by Verus: [SC-3] ensures all subsequent try_acquire calls observe sealed.
    #[inline]
    pub fn seal(&self) {
        // Publish closure to is_sealed/admission observers. Sealing is not a
        // drain: mark_waiting/active/try_seal_if_idle acquire completed releases.
        self.state.fetch_or(SEALED_BIT, Ordering::Release);
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
    ///
    /// Linearization Point: RMW update with `reopen_update`.
    /// Guaranteed by Verus: [SC-7] requires sealed && active == 0; [SC-8] resets to !sealed && !waiting && active == 0.
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

        pub(crate) fn new_sealed() -> Self {
            Self {
                state: AtomicUsize::new(SEALED_BIT),
            }
        }

        pub(crate) fn active(&self) -> usize {
            self.state.load(Ordering::Acquire) & ACTIVE_COUNT_MASK
        }

        pub(crate) fn is_sealed(&self) -> bool {
            self.state.load(Ordering::Acquire) & SEALED_BIT != 0
        }

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

        pub(crate) fn try_acquire(&self) -> Result<(), Sealed> {
            self.state
                .fetch_update(Ordering::Acquire, Ordering::Relaxed, acquire_update)
                .map(|_| ())
                .map_err(|_| Sealed)
        }

        pub(crate) fn try_release_without_notification(&self) -> Option<ReleaseOutcome> {
            self.state
                .fetch_update(
                    Ordering::Release,
                    Ordering::Relaxed,
                    release_without_notification_update,
                )
                .ok()
                .map(release_outcome)
        }

        pub(crate) fn release(&self) -> ReleaseOutcome {
            let previous = self
                .state
                .fetch_update(Ordering::Release, Ordering::Relaxed, release_update)
                .expect("model releases an admitted count");
            release_outcome(previous)
        }

        pub(crate) fn mark_waiting(&self) -> usize {
            self.state.fetch_or(WAITING_BIT, Ordering::AcqRel) & ACTIVE_COUNT_MASK
        }

        pub(crate) fn seal(&self) {
            self.state.fetch_or(SEALED_BIT, Ordering::Release);
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

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_idle_observer_acquires_all_holders_through_the_release_sequence() {
        use loom::sync::Arc;
        use loom::sync::atomic::AtomicUsize;
        use loom::thread;

        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.check(|| {
            let counter = Arc::new(loom_support::Counter::new());
            let values = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
            counter.try_acquire().unwrap();
            counter.try_acquire().unwrap();
            counter.seal();
            let mut holders = Vec::new();
            for index in 0..2 {
                let counter = Arc::clone(&counter);
                let values = Arc::clone(&values);
                holders.push(thread::spawn(move || {
                    values[index].store(index + 1, Ordering::Relaxed);
                    counter.release();
                }));
            }
            while counter.active() != 0 {
                thread::yield_now();
            }
            // Check before join: the counter must supply the synchronization.
            assert_eq!(values[0].load(Ordering::Relaxed), 1);
            assert_eq!(values[1].load(Ordering::Relaxed), 2);
            for holder in holders {
                holder.join().unwrap();
            }
        });
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_admission_acquires_reopened_generation_data() {
        use loom::sync::Arc;
        use loom::sync::atomic::AtomicUsize;
        use loom::thread;

        loom::model(|| {
            let counter = Arc::new(loom_support::Counter::new_sealed());
            let value = Arc::new(AtomicUsize::new(0));
            let reader_counter = Arc::clone(&counter);
            let reader_value = Arc::clone(&value);
            let reader = thread::spawn(move || {
                while reader_counter.try_acquire().is_err() {
                    thread::yield_now();
                }
                assert_eq!(reader_value.load(Ordering::Relaxed), 7);
                reader_counter.release();
            });
            value.store(7, Ordering::Relaxed);
            counter.reopen().unwrap();
            reader.join().unwrap();
        });
    }

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
