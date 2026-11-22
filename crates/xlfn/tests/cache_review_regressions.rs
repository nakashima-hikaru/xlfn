#![cfg(feature = "cache")]

use std::hash::{Hash, Hasher};
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
        // Detect any unexpected user hashing after computation.
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
fn resident_publication_reuses_the_prepared_hash_and_releases_the_value() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let key = OneShotPanickingKey {
        id: 7,
        panic_on_next_hash: Arc::clone(&armed),
    };
    let cache = CalculationCache::<OneShotPanickingKey, DropProbe>::new(
        xlfn::cache::CacheBudget::weight_units(1),
    );

    let lease = cache
        .get_or_try_insert_with(key, || {
            armed.store(true, Ordering::SeqCst);
            Ok(DropProbe(Arc::clone(&dropped)))
        })
        .expect("publication must reuse the prepared hash");
    assert!(
        armed.swap(false, Ordering::SeqCst),
        "publication consumed the armed hash"
    );
    drop(lease);
    cache.clear();
    drop(cache);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "clearing the published value must release its last pin exactly once"
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn a_usize_budget_must_not_silently_shrink_to_u32_max() {
    let budget = (u32::MAX as usize) + 1;
    let cache = CalculationCache::<u8, ()>::with_weigher(
        xlfn::cache::CacheBudget::weight_units(budget),
        |_| u32::MAX as usize + 1,
    );
    let lease = cache
        .get_or_try_insert_with(7, || Ok(()))
        .expect("computation succeeds");
    drop(lease);

    assert!(
        cache.get(&7).is_some(),
        "an entry whose weight equals the requested budget should be eligible"
    );
    assert_eq!(cache.used_weight(), budget);
}
