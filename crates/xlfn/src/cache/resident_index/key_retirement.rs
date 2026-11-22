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
                reclaim(std::iter::once(key));
            }
            _ => {
                // Detach a burst in one acquisition, without retaining its
                // possibly large allocation or destroying a key under lock.
                let batch = std::mem::take(&mut *keys);
                drop(keys);
                reclaim(batch);
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
            reclaim(keys);
        }
    }
}

// The two owned batch forms are a singleton iterator or a detached Vec.
// Both are independent of the queue; the deferred callback consumes each key.
fn reclaim<K, B: IntoIterator<Item = VersionedKey<K>>>(keys: B) {
    if super::super::destruction_deferred() {
        // SAFETY: resident keys originate only from CalculationCache with K: 'static.
        // The detached batch is a Vec or Once of those keys, owns all its
        // contents, and the callback contains each user destructor panic.
        unsafe { super::super::deferred_drop::defer(keys, reclaim::<K, B>) };
        return;
    }
    for key in keys {
        if catch_no_unwind(AssertUnwindSafe(|| drop(key))).is_err() {
            crate::diagnostics::report_no_unwind(
                "calculation cache key final drop",
                &crate::XllError::Panic,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_key_reuses_capacity_and_bursts_release_storage() {
        let retired = RetiredKeys::new();
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
