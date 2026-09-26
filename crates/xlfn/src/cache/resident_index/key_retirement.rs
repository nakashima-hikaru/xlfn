//! Keys may own arbitrary user resources, just like cached values. Backends
//! enqueue them under their locks; callers destroy detached batches afterward.

use super::VersionedKey;
use crate::panic_boundary::catch_no_unwind;
use crate::sync::Mutex;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct RetiredKeys<K> {
    pending: AtomicBool,
    keys: Mutex<Vec<VersionedKey<K>>>,
}

impl<K> RetiredKeys<K> {
    pub(super) fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            keys: Mutex::new(Vec::new()),
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
        reclaim(keys);
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

impl<K> Drop for RetiredKeys<K> {
    fn drop(&mut self) {
        reclaim(std::mem::take(self.keys.get_mut()));
    }
}

fn reclaim<K>(keys: Vec<VersionedKey<K>>) {
    for key in keys {
        if catch_no_unwind(AssertUnwindSafe(|| drop(key))).is_err() {
            crate::diagnostics::report_no_unwind(
                "calculation cache key final drop",
                &crate::XllError::Panic,
            );
        }
    }
}
