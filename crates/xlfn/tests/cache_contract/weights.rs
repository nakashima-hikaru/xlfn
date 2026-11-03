use xlfn::cache::CalculationCache;

#[test]
fn zero_weight_is_normalized_to_minimum() {
    let cache = CalculationCache::<String, String>::with_weigher(1024, |_| 0);
    assert_eq!(cache.used_weight(), 0);

    cache
        .get_or_try_insert_with("k1".to_string(), || Ok("val1".to_string()))
        .unwrap();

    // Weight 0 must be normalized to minimum weight 1
    assert_eq!(cache.used_weight(), 1);
}

#[test]
fn weights_accumulate_monotonically() {
    let cache = CalculationCache::<String, usize>::with_weigher(1024, |value| *value);
    assert_eq!(cache.used_weight(), 0);

    cache
        .get_or_try_insert_with("k1".to_string(), || Ok(10))
        .unwrap();
    assert_eq!(cache.used_weight(), 10);

    cache
        .get_or_try_insert_with("k2".to_string(), || Ok(25))
        .unwrap();
    assert_eq!(cache.used_weight(), 35);

    cache
        .get_or_try_insert_with("k3".to_string(), || Ok(5))
        .unwrap();
    assert_eq!(cache.used_weight(), 40);
}

#[test]
fn construction_policy_runs_once_per_created_value_and_unit_default_is_explicit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static WEIGHED: AtomicUsize = AtomicUsize::new(0);
    let cache = CalculationCache::<u8, String>::with_weigher(64, |value| {
        WEIGHED.fetch_add(1, Ordering::SeqCst);
        value.len()
    });
    let first = cache
        .get_or_try_insert_with(1, || Ok("bytes".into()))
        .unwrap();
    let hit = cache
        .get_or_try_insert_with(1, || panic!("warm hit recomputed"))
        .unwrap();
    assert_eq!(&*first, &*hit);
    assert_eq!(cache.used_weight(), 5);
    assert_eq!(WEIGHED.load(Ordering::SeqCst), 1);
    let units = CalculationCache::<u8, String>::new(64);
    drop(
        units
            .get_or_try_insert_with(1, || Ok("many bytes".into()))
            .unwrap(),
    );
    assert_eq!(units.used_weight(), 1);
}
