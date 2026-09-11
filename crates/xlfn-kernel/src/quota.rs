//! A generic bounded permit counter.

#![allow(
    unsafe_code,
    reason = "quota permits are non-owning capabilities whose owner is reclaimed after all permits"
)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::invariant::checked_atomic_dec_relaxed;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuotaExceeded;

pub struct Quota {
    // Capacity accounting only. The owner-lifetime contract, not observing
    // this count reach zero, synchronizes destruction of permit-bearing objects.
    used: AtomicUsize,
    limit: usize,
}

impl Quota {
    pub const fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit,
        }
    }

    /// Acquires a non-owning permit.
    ///
    /// # Safety
    ///
    /// The quota must outlive the returned permit. Its owner must drain or
    /// destroy every permit-bearing object before reclaiming the quota.
    pub unsafe fn try_acquire(&self) -> Result<QuotaPermit, QuotaExceeded> {
        self.used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                (used < self.limit).then(|| used + 1)
            })
            .map_err(|_| QuotaExceeded)?;

        Ok(QuotaPermit {
            quota: NonNull::from(self),
        })
    }

    #[inline]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }
}

pub struct QuotaPermit {
    quota: NonNull<Quota>,
}

impl Drop for QuotaPermit {
    fn drop(&mut self) {
        // SAFETY: guaranteed by `Quota::try_acquire`'s owner-lifetime
        // contract. Dropping the permit ends the capability before reclaim.
        let _ = checked_atomic_dec_relaxed(&unsafe { self.quota.as_ref() }.used);
    }
}

// SAFETY: Quota is thread-safe and the lifetime contract is independent of
// the thread on which a permit is dropped.
unsafe impl Send for QuotaPermit {}
// SAFETY: Quota is thread-safe and immutable borrows can be shared across threads.
unsafe impl Sync for QuotaPermit {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_maximum_quota_does_not_overflow() {
        let quota = Quota::new(usize::MAX);
        quota.used.store(usize::MAX, Ordering::Relaxed);
        // SAFETY: the quota stays alive through the attempt. No permit is
        // created for the synthetic exhausted state.
        assert!(unsafe { quota.try_acquire() }.is_err());
        assert_eq!(quota.used(), usize::MAX);
    }

    #[test]
    fn miri_quota_enforces_zero_and_bounded_limits_and_reuses_released_capacity() {
        let zero = Quota::new(0);
        // SAFETY: both quota owners outlive every permit in this test.
        assert!(unsafe { zero.try_acquire() }.is_err());
        let quota = Quota::new(1);
        // SAFETY: the permit is explicitly dropped before quota.
        let permit = unsafe { quota.try_acquire() }.unwrap();
        assert_eq!(quota.used(), 1);
        // SAFETY: the owner outlives this rejected attempt.
        assert!(unsafe { quota.try_acquire() }.is_err());
        drop(permit);
        assert_eq!(quota.used(), 0);
        // SAFETY: the temporary permit is dropped before quota.
        drop(unsafe { quota.try_acquire() }.unwrap());
        assert_eq!(quota.used(), 0);
    }
}
