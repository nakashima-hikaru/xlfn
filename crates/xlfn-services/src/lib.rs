//! Host-independent service backends used by xlfn.

#![deny(unsafe_code)]

#[allow(
    unsafe_code,
    reason = "handle table manages type-erased pinned storage and lease provenance"
)]
pub mod handle_objects;

#[cfg(feature = "cache")]
#[allow(
    unsafe_code,
    reason = "cache implements zero-copy slab residency, endpoint pinning, and reclamation"
)]
pub mod cache;
#[cfg(feature = "rtd")]
pub mod rtd;
#[cfg(feature = "cache")]
use xlfn_engine::panic_boundary;
#[cfg(feature = "cache")]
use xlfn_kernel::sync;
use xlfn_kernel::sync::Mutex;

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
pub struct Usage {
    pub subscriptions: usize,
    pub values: usize,
    pub bytes: usize,
    pub workers: usize,
}

struct State {
    limit: Usage,
    used: Usage,
}

/// Serializes checked admission for four independent resource classes.
pub struct Accounting {
    state: Mutex<State>,
}

impl Accounting {
    pub fn new(limit: Usage) -> Self {
        Self {
            state: Mutex::new(State {
                limit,
                used: Usage::default(),
            }),
        }
    }

    /// Returns a coherent observational snapshot, not an admission permit.
    pub fn usage(&self) -> Usage {
        self.state.lock().used
    }

    pub fn configure_source(&self, subscriptions: usize, values: usize, bytes: usize) {
        let mut state = self.state.lock();
        state.limit.subscriptions = subscriptions;
        state.limit.values = values;
        state.limit.bytes = bytes;
    }

    pub fn configure_worker_limit(&self, limit: usize) {
        self.state.lock().limit.workers = limit;
    }
    pub fn acquire(&self, amount: Usage) -> bool {
        let mut state = self.state.lock();
        let add = |used: usize, amount: usize, limit: usize| {
            // Lowering one limit must not stop unrelated resources already
            // admitted under it (for example, existing subscriptions publishing).
            if amount == 0 {
                Some(used)
            } else {
                used.checked_add(amount).filter(|next| *next <= limit)
            }
        };
        let Some(subscriptions) = add(
            state.used.subscriptions,
            amount.subscriptions,
            state.limit.subscriptions,
        ) else {
            return false;
        };
        let Some(values) = add(state.used.values, amount.values, state.limit.values) else {
            return false;
        };
        let Some(bytes) = add(state.used.bytes, amount.bytes, state.limit.bytes) else {
            return false;
        };
        let Some(workers) = add(state.used.workers, amount.workers, state.limit.workers) else {
            return false;
        };
        state.used = Usage {
            subscriptions,
            values,
            bytes,
            workers,
        };
        true
    }

    #[inline]
    pub fn acquire_buffered(&self, values: usize, bytes: usize) -> bool {
        let mut state = self.state.lock();
        let Some(next_values) = state.used.values.checked_add(values) else {
            return false;
        };
        let Some(next_bytes) = state.used.bytes.checked_add(bytes) else {
            return false;
        };
        if (values != 0 && next_values > state.limit.values)
            || (bytes != 0 && next_bytes > state.limit.bytes)
        {
            return false;
        }
        state.used.values = next_values;
        state.used.bytes = next_bytes;
        true
    }

    #[inline]
    pub fn release_buffered(&self, values: usize, bytes: usize) {
        let mut state = self.state.lock();
        state.used.values = xlfn_kernel::invariant::checked_sub_or_abort(state.used.values, values);
        state.used.bytes = xlfn_kernel::invariant::checked_sub_or_abort(state.used.bytes, bytes);
    }

    pub fn release(&self, amount: Usage) {
        let mut state = self.state.lock();
        state.used.subscriptions = xlfn_kernel::invariant::checked_sub_or_abort(
            state.used.subscriptions,
            amount.subscriptions,
        );
        state.used.values =
            xlfn_kernel::invariant::checked_sub_or_abort(state.used.values, amount.values);
        state.used.bytes =
            xlfn_kernel::invariant::checked_sub_or_abort(state.used.bytes, amount.bytes);
        state.used.workers =
            xlfn_kernel::invariant::checked_sub_or_abort(state.used.workers, amount.workers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_resource_admission_is_atomic_and_lowered_limits_are_independent() {
        let ledger = Accounting::new(Usage {
            subscriptions: 1,
            values: 2,
            bytes: 8,
            workers: 1,
        });
        assert!(ledger.acquire(Usage {
            subscriptions: 1,
            ..Usage::default()
        }));
        ledger.configure_source(0, 2, 8);
        assert!(!ledger.acquire(Usage {
            subscriptions: 1,
            values: 1,
            ..Usage::default()
        }));
        assert_eq!(ledger.usage().values, 0);
        assert!(!ledger.acquire_buffered(1, 9));
        assert!(ledger.acquire_buffered(2, 8));
        ledger.release_buffered(2, 8);
        ledger.release(Usage {
            subscriptions: 1,
            ..Usage::default()
        });
        assert_eq!(ledger.usage(), Usage::default());
    }
}
