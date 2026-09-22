use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use xlfn::cache::{CacheEndpoint, CacheRegistry, CanonicalF64};

struct MarkerA;
struct MarkerB;

#[test]
fn different_endpoint_markers_remain_distinct() {
    let registry = CacheRegistry::new(1024);
    let ep_a = CacheEndpoint::<String, String, MarkerA>::new("endpoint");
    let ep_b = CacheEndpoint::<String, String, MarkerB>::new("endpoint");

    registry
        .get_or_try_insert(&ep_a, "k1".to_string(), |_| 1, || Ok("val_a".to_string()))
        .unwrap();

    assert_eq!(
        &*registry.get(&ep_a, &"k1".to_string()).unwrap().unwrap(),
        "val_a"
    );
    assert!(registry.get(&ep_b, &"k1".to_string()).unwrap().is_none());
}

#[test]
fn versioned_endpoint_ids_are_distinct() {
    let registry = CacheRegistry::new(1024);
    let ep1 = CacheEndpoint::<String, String, MarkerA>::new("v1");
    let ep2 = CacheEndpoint::<String, String, MarkerA>::new("v2");

    ep1.get_or_try_insert(
        &registry,
        "k1".to_string(),
        |_| 1,
        || Ok("val1".to_string()),
    )
    .unwrap();

    assert_eq!(
        &*ep1.get(&registry, &"k1".to_string()).unwrap().unwrap(),
        "val1"
    );
    assert!(ep2.get(&registry, &"k1".to_string()).unwrap().is_none());
}

#[test]
fn registry_clear_clears_all_endpoints() {
    let registry = CacheRegistry::new(1024);
    let ep_a = CacheEndpoint::<String, String, MarkerA>::new("ep_a");
    let ep_b = CacheEndpoint::<String, String, MarkerB>::new("ep_b");

    registry
        .get_or_try_insert(&ep_a, "k1".to_string(), |_| 1, || Ok("val_a".to_string()))
        .unwrap();
    registry
        .get_or_try_insert(&ep_b, "k2".to_string(), |_| 1, || Ok("val_b".to_string()))
        .unwrap();

    assert!(registry.get(&ep_a, &"k1".to_string()).unwrap().is_some());
    assert!(registry.get(&ep_b, &"k2".to_string()).unwrap().is_some());

    registry.clear();

    assert!(registry.get(&ep_a, &"k1".to_string()).unwrap().is_none());
    assert!(registry.get(&ep_b, &"k2".to_string()).unwrap().is_none());
}

#[test]
fn canonical_f64_normalizes_zero() {
    let pos_zero = CanonicalF64::new(0.0).unwrap();
    let neg_zero = CanonicalF64::new(-0.0).unwrap();

    assert_eq!(pos_zero, neg_zero);
    assert_eq!(pos_zero.get(), 0.0);
    assert_eq!(neg_zero.get(), 0.0);

    let mut h1 = DefaultHasher::new();
    pos_zero.hash(&mut h1);
    let mut h2 = DefaultHasher::new();
    neg_zero.hash(&mut h2);
    assert_eq!(h1.finish(), h2.finish());
}

#[test]
fn canonical_f64_rejects_nan_and_inf() {
    assert!(CanonicalF64::new(f64::NAN).is_err());
    assert!(CanonicalF64::new(f64::INFINITY).is_err());
    assert!(CanonicalF64::new(f64::NEG_INFINITY).is_err());
    assert!(CanonicalF64::new(42.5).is_ok());
}
