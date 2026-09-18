#![cfg(feature = "cache")]

use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn::cache::CalculationCache;

#[derive(Clone)]
struct OneShotPanickingKey {
    id: u8,
    panic_on_next_hash: Arc<AtomicBool>,
}

impl PartialEq for OneShotPanickingKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for OneShotPanickingKey {}

impl Hash for OneShotPanickingKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // One-shot fault injection: cleanup is allowed to hash the key again.
        if self.panic_on_next_hash.swap(false, Ordering::SeqCst) {
            panic!("injected hash panic after computation");
        }
        self.id.hash(state);
    }
}

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn post_compute_hash_panic_must_not_leak_the_creator_pin() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let key = OneShotPanickingKey {
        id: 7,
        panic_on_next_hash: Arc::clone(&armed),
    };
    let cache = CalculationCache::<OneShotPanickingKey, DropProbe>::new(1);

    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let result = cache.get_or_try_insert_with(
            key,
            |_| 2, // Overweight: exercises post-compute invalidation.
            || {
                // Initial lookups succeeded; the next hash now fails after
                // an application value has been created.
                armed.store(true, Ordering::SeqCst);
                Ok(DropProbe(Arc::clone(&dropped)))
            },
        );
        drop(result);
    }));

    assert!(outcome.is_err(), "the injected hash must have executed");
    cache.clear();
    drop(cache);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "unwinding must release the creator pin, not merely drain queued nodes"
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn a_usize_budget_must_not_silently_shrink_to_u32_max() {
    let budget = (u32::MAX as usize) + 1;
    let cache = CalculationCache::<u8, ()>::new(budget);
    let lease = cache
        .get_or_try_insert_with(7, |_| budget, || Ok(()))
        .expect("computation succeeds");
    drop(lease);

    assert!(
        cache.get(&7).is_some(),
        "an entry whose weight equals the requested budget should be eligible"
    );
    assert_eq!(cache.used_weight(), budget);
}
