use xlfn::cache::CalculationCache;

#[test]
fn clear_invalidates_resident_values() {
    let cache = CalculationCache::<String, String>::new(1024);
    cache
        .get_or_try_insert_with("k1".to_string(), |_| 1, || Ok("v1".to_string()))
        .unwrap();
    assert_eq!(cache.len(), 1);

    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.get(&"k1".to_string()).is_none());
}

#[test]
fn lease_survives_clear() {
    let cache = CalculationCache::<String, String>::new(1024);
    let lease = cache
        .get_or_try_insert_with("k1".to_string(), |_| 1, || Ok("v1".to_string()))
        .unwrap();

    cache.clear();

    assert!(cache.get(&"k1".to_string()).is_none());
    assert_eq!(&*lease, "v1");
}
