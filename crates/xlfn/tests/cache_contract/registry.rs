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
        .get_or_try_insert(&ep_a, "k1".to_string(), || Ok("val_a".to_string()))
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

    registry
        .get_or_try_insert(&ep1, "k1".to_string(), || Ok("val1".to_string()))
        .unwrap();

    assert_eq!(
        &*registry.get(&ep1, &"k1".to_string()).unwrap().unwrap(),
        "val1"
    );
    assert!(registry.get(&ep2, &"k1".to_string()).unwrap().is_none());
}

#[test]
fn registry_clear_clears_all_endpoints() {
    let registry = CacheRegistry::new(1024);
    let ep_a = CacheEndpoint::<String, String, MarkerA>::new("ep_a");
    let ep_b = CacheEndpoint::<String, String, MarkerB>::new("ep_b");

    registry
        .get_or_try_insert(&ep_a, "k1".to_string(), || Ok("val_a".to_string()))
        .unwrap();
    registry
        .get_or_try_insert(&ep_b, "k2".to_string(), || Ok("val_b".to_string()))
        .unwrap();

    assert!(registry.get(&ep_a, &"k1".to_string()).unwrap().is_some());
    assert!(registry.get(&ep_b, &"k2".to_string()).unwrap().is_some());

    registry.clear();

    assert!(registry.get(&ep_a, &"k1".to_string()).unwrap().is_none());
    assert!(registry.get(&ep_b, &"k2".to_string()).unwrap().is_none());
}

#[test]
fn endpoint_fixes_resource_units_for_all_callers_and_rejects_conflicts() {
    static BYTES: CacheEndpoint<u32, String> =
        CacheEndpoint::new("accounting-v1").with_weigher(String::len);
    let registry = CacheRegistry::new(4);
    drop(
        registry
            .get_or_try_insert(&BYTES, 1, || Ok("four".into()))
            .unwrap(),
    );
    // A second caller uses the same policy, even for a different key. Five
    // bytes exceed the fixed budget and must not become resident.
    let oversized = registry
        .get_or_try_insert(&BYTES, 2, || Ok("large".into()))
        .unwrap();
    assert_eq!(&*oversized, "large");
    assert!(registry.get(&BYTES, &2).unwrap().is_none());

    let units = CacheEndpoint::<u32, String>::new("accounting-v1");
    // Warm TLS resolution must still reject a conflicting descriptor.
    assert!(registry.get(&units, &1).is_err());
    assert!(
        registry
            .get_or_try_insert(&units, 3, || Ok("other".into()))
            .is_err()
    );
    std::thread::scope(|scope| {
        // A fresh thread also checks policy on its cold shared-index path.
        scope
            .spawn(|| assert!(registry.get(&units, &1).is_err()))
            .join()
            .unwrap();
    });
    registry.clear();
    assert!(registry.get(&units, &1).is_err());
    assert!(registry.get(&BYTES, &1).unwrap().is_none());
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
