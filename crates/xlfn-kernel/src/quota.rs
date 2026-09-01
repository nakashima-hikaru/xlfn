//! A generic bounded permit counter.

#![allow(
    unsafe_code,
    reason = "quota permits are non-owning capabilities whose owner is reclaimed after all permits"
)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::invariant::checked_atomic_dec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuotaExceeded;

pub struct Quota {
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
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < self.limit).then_some(used + 1)
            })
            .map_err(|_| QuotaExceeded)?;

        Ok(QuotaPermit {
            quota: NonNull::from(self),
        })
    }

    #[inline]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

pub struct QuotaPermit {
    quota: NonNull<Quota>,
}

impl Drop for QuotaPermit {
    fn drop(&mut self) {
        // SAFETY: guaranteed by `Quota::try_acquire`'s owner-lifetime
        // contract. Dropping the permit ends the capability before reclaim.
        let _ = checked_atomic_dec(&unsafe { self.quota.as_ref() }.used);
    }
}

// SAFETY: Quota is thread-safe and the lifetime contract is independent of
// the thread on which a permit is dropped.
unsafe impl Send for QuotaPermit {}
unsafe impl Sync for QuotaPermit {}
