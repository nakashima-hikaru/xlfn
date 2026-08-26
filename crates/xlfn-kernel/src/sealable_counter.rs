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

/// A bounded active counter with a single sealed bit.
///
/// This type deliberately knows nothing about lifecycle phases, generations,
/// or the resource represented by a permit.  It only provides the mechanical
/// acquire/seal/release/reopen protocol used by higher-level domains.
pub struct SealableCounter {
    state: AtomicUsize,
}

pub const SEALED_BIT: usize = 1_usize << (usize::BITS - 1);
const ACTIVE_COUNT_MASK: usize = SEALED_BIT - 1;

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
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                if state & SEALED_BIT != 0 {
                    return None;
                }
                if state & ACTIVE_COUNT_MASK == ACTIVE_COUNT_MASK {
                    fail_stop();
                }
                Some(state + 1)
            })
            .map(|_| ())
            .map_err(|_| Sealed)
    }

    #[inline]
    pub fn release(&self) -> ReleaseOutcome {
        let previous = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let active = state & ACTIVE_COUNT_MASK;
                (active != 0).then_some(state - 1)
            })
            .unwrap_or_else(|_| fail_stop());

        if previous & ACTIVE_COUNT_MASK == 1 {
            ReleaseOutcome::BecameIdle
        } else {
            ReleaseOutcome::StillActive
        }
    }

    #[inline]
    pub fn seal(&self) {
        self.state.fetch_or(SEALED_BIT, Ordering::AcqRel);
    }

    pub fn reopen(&self) -> Result<(), ReopenError> {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                if state & SEALED_BIT == 0 || state & ACTIVE_COUNT_MASK != 0 {
                    None
                } else {
                    Some(0)
                }
            })
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
}
