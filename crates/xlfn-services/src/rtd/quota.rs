use xlfn_kernel::invariant::checked_sub_or_abort;

/// Ledger for pending subscriptions and retained topic bytes, under the
/// catalog's lock. Active entries can still count as pending until their
/// final preparation ticket is returned.
#[derive(Default)]
pub struct CatalogQuota {
    pending: usize,
    topic_bytes: usize,
}

impl CatalogQuota {
    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn topic_bytes(&self) -> usize {
        self.topic_bytes
    }

    pub fn prepare_insert(
        &mut self,
        bytes: usize,
        max_pending: usize,
        max_topic_bytes: usize,
    ) -> Option<PendingCharge<'_>> {
        if self.pending >= max_pending {
            return None;
        }
        let topic_bytes = self
            .topic_bytes
            .checked_add(bytes)
            .filter(|&total| total <= max_topic_bytes)?;

        Some(PendingCharge {
            quota: self,
            total_topic_bytes: topic_bytes,
        })
    }

    pub fn transition(&mut self, was_pending: bool, is_pending: bool) {
        match (was_pending, is_pending) {
            (false, true) => {
                self.pending = self
                    .pending
                    .checked_add(1)
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            }
            (true, false) => self.pending = checked_sub_or_abort(self.pending, 1),
            _ => {}
        }
    }

    pub fn remove(&mut self, bytes: usize, was_pending: bool) {
        self.transition(was_pending, false);
        self.topic_bytes = checked_sub_or_abort(self.topic_bytes, bytes);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Checked quota update. Dropping it before commit changes no counters.
pub struct PendingCharge<'a> {
    quota: &'a mut CatalogQuota,
    total_topic_bytes: usize,
}

impl PendingCharge<'_> {
    pub fn commit(self) {
        self.quota.transition(false, true);
        self.quota.topic_bytes = self.total_topic_bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoned_or_rejected_charge_never_changes_either_counter() {
        let mut quota = CatalogQuota::default();
        assert!(quota.prepare_insert(1, 0, 10).is_none());
        assert!(quota.prepare_insert(11, 1, 10).is_none());
        {
            let _abandoned = quota.prepare_insert(4, 1, 8).unwrap();
        }
        assert_eq!((quota.pending(), quota.topic_bytes()), (0, 0));
        quota.prepare_insert(4, 1, 8).unwrap().commit();
        assert!(quota.prepare_insert(1, 1, 8).is_none());
        assert!(quota.prepare_insert(usize::MAX, 2, usize::MAX).is_none());
        quota.transition(true, false);
        assert_eq!((quota.pending(), quota.topic_bytes()), (0, 4));
        quota.prepare_insert(4, 1, 8).unwrap().commit();
        quota.remove(4, false);
        assert_eq!((quota.pending(), quota.topic_bytes()), (1, 4));
        quota.remove(4, true);
        assert_eq!((quota.pending(), quota.topic_bytes()), (0, 0));
    }
}
