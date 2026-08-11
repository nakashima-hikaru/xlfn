use super::*;

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

    pub(crate) fn try_acquire(self: &Arc<Self>) -> XllResult<QuotaPermit> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < self.limit).then_some(used + 1)
            })
            .map_err(|_| XllError::Overloaded)?;

        Ok(QuotaPermit {
            quota: Arc::clone(self),
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
