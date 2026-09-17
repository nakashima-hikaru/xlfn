use xlfn::cache::CalculationCache;

#[test]
fn overweight_result_returned_but_not_resident() {
    let cache = CalculationCache::<String, String>::new(10);

    let lease = cache
        .get_or_try_insert_with(
            "huge".to_string(),
            |_| 50,
            || Ok("huge_payload".to_string()),
        )
        .expect("overweight initialization should return value to caller");

    assert_eq!(&*lease, "huge_payload");
    assert_eq!(cache.len(), 0);
    assert!(cache.get(&"huge".to_string()).is_none());
}
