//! A generic bounded permit counter.

use std::sync::atomic::{AtomicUsize, Ordering};
use triomphe::Arc;

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

    pub fn try_acquire(this: &Arc<Self>) -> Result<QuotaPermit, QuotaExceeded> {
        this.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < this.limit).then_some(used + 1)
            })
            .map_err(|_| QuotaExceeded)?;

        Ok(QuotaPermit {
            quota: Arc::clone(this),
        })
    }

    #[inline]
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

pub struct QuotaPermit {
    quota: Arc<Quota>,
}

impl Drop for QuotaPermit {
    fn drop(&mut self) {
        let _ = checked_atomic_dec(&self.quota.used);
    }
}
