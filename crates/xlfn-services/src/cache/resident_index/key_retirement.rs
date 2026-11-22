//! Keys may own arbitrary user resources, just like cached values. Backends
//! enqueue them under their locks; callers destroy detached batches afterward.

use super::{CachePanicReporter, VersionedKey};
use crate::cache::report_drop_panic;
use crate::panic_boundary::catch_no_unwind;
use crate::sync::Mutex;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct RetiredKeys<K> {
    pending: AtomicBool,
    keys: Mutex<Vec<VersionedKey<K>>>,
    report: CachePanicReporter,
}

impl<K> RetiredKeys<K> {
    pub(super) fn new(report: CachePanicReporter) -> Self {
        Self {
            pending: AtomicBool::new(false),
            keys: Mutex::new(Vec::new()),
            report,
        }
    }

    pub(super) fn push(&self, key: VersionedKey<K>) {
        let mut keys = self.keys.lock();
        keys.push(key);
        self.pending.store(true, Ordering::Release);
    }

    pub(super) fn reclaim(&self) {
        if !self.has_pending() {
            return;
        }
        let keys = {
            let mut keys = self.keys.lock();
            self.pending.store(false, Ordering::Relaxed);
            std::mem::take(&mut *keys)
        };
        reclaim((keys, self.report));
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

impl<K> Drop for RetiredKeys<K> {
    fn drop(&mut self) {
        reclaim((std::mem::take(self.keys.get_mut()), self.report));
    }
}

type DropBatch<K> = (Vec<VersionedKey<K>>, CachePanicReporter);

fn reclaim<K>((keys, report): DropBatch<K>) {
    if super::super::destruction_deferred() && !keys.is_empty() {
        // SAFETY: resident keys originate only from CalculationCache with K: 'static.
        // This detached owned batch does not borrow the index or its queue.
        unsafe { super::super::deferred_drop::defer((keys, report), reclaim::<K>) };
        return;
    }
    for key in keys {
        if catch_no_unwind(AssertUnwindSafe(|| drop(key))).is_err() {
            report_drop_panic(report, "calculation cache key final drop");
        }
    }
}
