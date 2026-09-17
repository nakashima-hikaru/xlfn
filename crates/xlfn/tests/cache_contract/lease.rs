use xlfn::cache::CalculationCache;

#[test]
fn lease_survives_eviction() {
    let cache = CalculationCache::<u32, String>::new(8);

    let lease1 = cache
        .get_or_try_insert_with(1, |_| 8, || Ok("val1".to_string()))
        .unwrap();
    let lease2 = cache
        .get_or_try_insert_with(2, |_| 8, || Ok("val2".to_string()))
        .unwrap();

    // With total weight capacity 8, at most one item of weight 8 can remain resident.
    assert!(cache.len() <= 1);
    assert!(cache.get(&1).is_none() || cache.get(&2).is_none());

    // Leases for both items remain valid and accessible regardless of which was evicted.
    assert_eq!(&*lease1, "val1");
    assert_eq!(&*lease2, "val2");
}
