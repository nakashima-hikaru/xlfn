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

    fn acquire_count(&self) -> Result<(), QuotaExceeded> {
        self.used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                (used < self.limit).then(|| used + 1)
            })
            .map_err(|_| QuotaExceeded)?;

        Ok(())
    }

    /// Borrows the quota for the full lifetime of a non-cloneable permit.
    pub fn try_acquire(&self) -> Result<QuotaPermit<'_>, QuotaExceeded> {
        self.acquire_count()?;
        Ok(QuotaPermit { quota: self })
    }

    /// Acquires a permit stored in an object retained by the quota's owner.
    ///
    /// # Safety
    /// The owner must outlive every permit, including its release operation.
    /// `used() == 0` is capacity accounting and is not a destruction barrier.
    pub unsafe fn try_acquire_detached(&self) -> Result<DetachedQuotaPermit, QuotaExceeded> {
        self.acquire_count()?;
        Ok(DetachedQuotaPermit {
            quota: NonNull::from(self),
        })
    }

    #[inline]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }
}

#[must_use = "retain the permit for the capacity reservation"]
pub struct QuotaPermit<'quota> {
    quota: &'quota Quota,
}

impl Drop for QuotaPermit<'_> {
    fn drop(&mut self) {
        let _ = checked_atomic_dec_relaxed(&self.quota.used);
    }
}

/// Lifetime-erased reservation whose owner must satisfy the unsafe acquisition
/// contract. There is deliberately no safe conversion from a borrowed permit.
#[must_use = "retain the detached permit until its owner-bound operation ends"]
pub struct DetachedQuotaPermit {
    quota: NonNull<Quota>,
}

impl Drop for DetachedQuotaPermit {
    fn drop(&mut self) {
        // SAFETY: guaranteed by `Quota::try_acquire_detached`'s owner-lifetime
        // contract. Dropping the permit ends the capability before reclaim.
        let _ = checked_atomic_dec_relaxed(&unsafe { self.quota.as_ref() }.used);
    }
}

// SAFETY: Quota is thread-safe and the lifetime contract is independent of
// the thread on which a permit is dropped.
unsafe impl Send for DetachedQuotaPermit {}
// SAFETY: Quota is thread-safe and immutable borrows can be shared across threads.
unsafe impl Sync for DetachedQuotaPermit {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_maximum_quota_does_not_overflow() {
        let quota = Quota::new(usize::MAX);
        quota.used.store(usize::MAX, Ordering::Relaxed);
        // No permit is created for the synthetic exhausted state.
        assert!(quota.try_acquire().is_err());
        assert_eq!(quota.used(), usize::MAX);
    }

    #[test]
    fn miri_detached_quota_reservation_requires_explicit_owner_retention() {
        let quota = Quota::new(1);
        // SAFETY: this stack-owned quota is retained until its detached permit
        // is explicitly destroyed. No zero-count reclamation is used.
        let permit = unsafe { quota.try_acquire_detached() }.unwrap();
        assert_eq!(quota.used(), 1);
        drop(permit);
        assert_eq!(quota.used(), 0);
    }

    #[test]
    fn miri_quota_enforces_zero_and_bounded_limits_and_reuses_released_capacity() {
        let zero = Quota::new(0);
        assert!(zero.try_acquire().is_err());
        let quota = Quota::new(1);
        let permit = quota.try_acquire().unwrap();
        assert_eq!(quota.used(), 1);
        assert!(quota.try_acquire().is_err());
        drop(permit);
        assert_eq!(quota.used(), 0);
        drop(quota.try_acquire().unwrap());
        assert_eq!(quota.used(), 0);
    }
}
