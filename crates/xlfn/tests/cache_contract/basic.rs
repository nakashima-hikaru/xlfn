use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::cache::CalculationCache;

#[test]
fn miss_computes_and_returns_lease() {
    let cache = CalculationCache::<String, String>::new(1024);
    assert_eq!(cache.len(), 0);
    assert!(cache.get(&"k1".to_string()).is_none());

    let lease = cache
        .get_or_try_insert_with("k1".to_string(), |v| v.len(), || Ok("val1".to_string()))
        .expect("computation should succeed");
    assert_eq!(&*lease, "val1");
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.used_weight(), 4);
}

#[test]
fn hit_does_not_recompute() {
    let cache = CalculationCache::<String, String>::new(1024);
    let computes = AtomicUsize::new(0);

    let lease1 = cache
        .get_or_try_insert_with(
            "k1".to_string(),
            |v| v.len(),
            || {
                computes.fetch_add(1, Ordering::SeqCst);
                Ok("val1".to_string())
            },
        )
        .unwrap();
    assert_eq!(&*lease1, "val1");
    assert_eq!(computes.load(Ordering::SeqCst), 1);

    let lease2 = cache
        .get_or_try_insert_with(
            "k1".to_string(),
            |v| v.len(),
            || {
                computes.fetch_add(1, Ordering::SeqCst);
                Ok("val2_should_not_run".to_string())
            },
        )
        .unwrap();
    assert_eq!(&*lease2, "val1");
    assert_eq!(computes.load(Ordering::SeqCst), 1);

    let get_lease = cache.get(&"k1".to_string()).unwrap();
    assert_eq!(&*get_lease, "val1");
}
