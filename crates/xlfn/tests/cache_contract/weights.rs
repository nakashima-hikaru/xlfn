use xlfn::cache::CalculationCache;

#[test]
fn zero_weight_is_normalized_to_minimum() {
    let cache = CalculationCache::<String, String>::new(1024);
    assert_eq!(cache.used_weight(), 0);

    cache
        .get_or_try_insert_with("k1".to_string(), |_| 0, || Ok("val1".to_string()))
        .unwrap();

    // Weight 0 must be normalized to minimum weight 1
    assert_eq!(cache.used_weight(), 1);
}

#[test]
fn weights_accumulate_monotonically() {
    let cache = CalculationCache::<String, String>::new(1024);
    assert_eq!(cache.used_weight(), 0);

    cache
        .get_or_try_insert_with("k1".to_string(), |_| 10, || Ok("val1".to_string()))
        .unwrap();
    assert_eq!(cache.used_weight(), 10);

    cache
        .get_or_try_insert_with("k2".to_string(), |_| 25, || Ok("val2".to_string()))
        .unwrap();
    assert_eq!(cache.used_weight(), 35);

    cache
        .get_or_try_insert_with("k3".to_string(), |_| 5, || Ok("val3".to_string()))
        .unwrap();
    assert_eq!(cache.used_weight(), 40);
}
