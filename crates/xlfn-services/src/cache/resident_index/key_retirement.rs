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
        if keys.capacity() == 0 {
            // Allocate one slot lazily, not Vec's larger first-growth batch.
            // A caller may choose a very large K.
            keys.reserve_exact(1);
        }
        keys.push(key);
        self.pending.store(true, Ordering::Release);
    }

    pub(super) fn reclaim(&self) {
        if !self.has_pending() {
            return;
        }
        let mut keys = self.keys.lock();
        self.pending.store(false, Ordering::Relaxed);
        match keys.len() {
            0 => {}
            1 => {
                // Keep the storage through repeated single-key evictions,
                // without embedding an arbitrary-size K in the queue.
                let key = keys.pop().expect("there is one retired key");
                drop(keys);
                reclaim((std::iter::once(key), self.report));
            }
            _ => {
                // Detach a burst in one acquisition, without retaining its
                // possibly large allocation or destroying a key under lock.
                let batch = std::mem::take(&mut *keys);
                drop(keys);
                reclaim((batch, self.report));
            }
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
}

impl<K> Drop for RetiredKeys<K> {
    fn drop(&mut self) {
        let keys = std::mem::take(self.keys.get_mut());
        if !keys.is_empty() {
            reclaim((keys, self.report));
        }
    }
}

pub(in crate::cache) fn retire_single_key<K: 'static>(
    key: VersionedKey<K>,
    report: CachePanicReporter,
) {
    reclaim((std::iter::once(key), report));
}

// The two owned batch forms are a singleton iterator or a detached Vec.
// Both are independent of the queue; the deferred callback consumes each key.
fn reclaim<K, B: IntoIterator<Item = VersionedKey<K>>>((keys, report): (B, CachePanicReporter)) {
    if super::super::destruction_deferred() {
        // SAFETY: resident keys originate only from CalculationCache with K: 'static.
        // The detached batch is a Vec or Once of those keys, owns all its
        // contents, and the callback contains each user destructor panic.
        unsafe { super::super::deferred_drop::defer((keys, report), reclaim::<K, B>) };
        return;
    }
    for key in keys {
        if catch_no_unwind(AssertUnwindSafe(|| drop(key))).is_err() {
            report_drop_panic(report, "calculation cache key final drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singleton_and_bulk_retirements_report_after_deferred_destruction() {
        use crate::cache::CacheOperationGuard;
        use std::sync::atomic::AtomicUsize;
        use xlfn_engine::error::XllError;

        static REPORTS: AtomicUsize = AtomicUsize::new(0);
        fn report(context: &'static str, error: &XllError) {
            if context == "calculation cache key final drop" && error.is_panic() {
                REPORTS.fetch_add(1, Ordering::Relaxed);
            }
        }
        struct PanickingKey;
        impl Drop for PanickingKey {
            fn drop(&mut self) {
                panic!("contained retired key drop");
            }
        }

        let retired = RetiredKeys::new(report);
        for count in [1, 2] {
            let before = REPORTS.load(Ordering::Relaxed);
            let operation = CacheOperationGuard::enter();
            for epoch in 0..count {
                retired.push(VersionedKey {
                    epoch,
                    key: PanickingKey,
                });
            }
            retired.reclaim();
            assert!(!retired.has_pending());
            assert_eq!(REPORTS.load(Ordering::Relaxed), before);
            drop(operation);
            assert_eq!(REPORTS.load(Ordering::Relaxed), before + count as usize);
        }
    }

    #[test]
    fn single_key_reuses_capacity_and_bursts_release_storage() {
        let retired = RetiredKeys::new(super::super::super::ignore_cache_panic);
        let push = |epoch: u64| {
            retired.push(VersionedKey {
                epoch,
                key: epoch.to_string(),
            });
            assert!(retired.has_pending());
        };
        push(0);
        let initial_storage = retired.keys.lock().as_ptr();
        retired.reclaim();
        for epoch in 1..4 {
            push(epoch);
            assert_eq!(retired.keys.lock().as_ptr(), initial_storage);
            retired.reclaim();
            assert!(!retired.has_pending());
            assert!(retired.keys.lock().capacity() > 0);
        }
        push(4);
        push(5);
        retired.reclaim();
        assert!(!retired.has_pending());
        assert_eq!(retired.keys.lock().capacity(), 0);
    }
}
