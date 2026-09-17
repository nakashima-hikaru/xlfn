use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use xlfn::cache::{CacheEndpoint, CacheRegistry, CanonicalF64};

struct MarkerA;
struct MarkerB;

#[test]
fn different_endpoint_markers_remain_distinct() {
    let registry = CacheRegistry::new(1024);
    let ep_a = CacheEndpoint::<MarkerA, String, String>::new("endpoint");
    let ep_b = CacheEndpoint::<MarkerB, String, String>::new("endpoint");

    let bound_a = registry.bind(&ep_a).unwrap();
    let bound_b = registry.bind(&ep_b).unwrap();

    bound_a
        .get_or_try_insert("k1".to_string(), |_| 1, || Ok("val_a".to_string()))
        .unwrap();

    assert_eq!(&*bound_a.get(&"k1".to_string()).unwrap(), "val_a");
    assert!(bound_b.get(&"k1".to_string()).is_none());
}

#[test]
fn versioned_endpoint_ids_are_distinct() {
    let registry = CacheRegistry::new(1024);
    let ep1 = CacheEndpoint::<MarkerA, String, String>::new("v1");
    let ep2 = CacheEndpoint::<MarkerA, String, String>::new("v2");

    let bound1 = registry.bind(&ep1).unwrap();
    let bound2 = registry.bind(&ep2).unwrap();

    bound1
        .get_or_try_insert("k1".to_string(), |_| 1, || Ok("val1".to_string()))
        .unwrap();

    assert_eq!(&*bound1.get(&"k1".to_string()).unwrap(), "val1");
    assert!(bound2.get(&"k1".to_string()).is_none());
}

#[test]
fn registry_clear_clears_all_endpoints() {
    let registry = CacheRegistry::new(1024);
    let ep_a = CacheEndpoint::<MarkerA, String, String>::new("ep_a");
    let ep_b = CacheEndpoint::<MarkerB, String, String>::new("ep_b");

    let bound_a = registry.bind(&ep_a).unwrap();
    let bound_b = registry.bind(&ep_b).unwrap();

    bound_a
        .get_or_try_insert("k1".to_string(), |_| 1, || Ok("val_a".to_string()))
        .unwrap();
    bound_b
        .get_or_try_insert("k2".to_string(), |_| 1, || Ok("val_b".to_string()))
        .unwrap();

    assert!(bound_a.get(&"k1".to_string()).is_some());
    assert!(bound_b.get(&"k2".to_string()).is_some());

    registry.clear();

    assert!(bound_a.get(&"k1".to_string()).is_none());
    assert!(bound_b.get(&"k2".to_string()).is_none());
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
