use crate::{XllError, XllResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use triomphe::Arc;

pub(crate) struct Quota {
    pub(crate) used: AtomicUsize,
    pub(crate) limit: usize,
}

impl Quota {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            limit,
        }
    }

    pub(crate) fn try_acquire(this: &Arc<Self>) -> XllResult<QuotaPermit> {
        this.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < this.limit).then_some(used + 1)
            })
            .map_err(|_| XllError::Overloaded)?;

        Ok(QuotaPermit {
            quota: Arc::clone(this),
        })
    }
}

pub(crate) struct QuotaPermit {
    pub(crate) quota: Arc<Quota>,
}

impl Drop for QuotaPermit {
    fn drop(&mut self) {
        let previous = self.quota.used.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous != 0, "quota permit drop underflow");
    }
}
