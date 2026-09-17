use xlfn::XllError;
use xlfn::cache::CalculationCache;
use xlfn::error::DomainErrorCode;

#[test]
fn compute_err_is_not_cached() {
    let cache = CalculationCache::<String, String>::new(1024);

    let res = cache.get_or_try_insert_with(
        "err_key".to_string(),
        |v| v.len(),
        || {
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
        },
    );
    assert!(res.is_err());
    assert_eq!(cache.len(), 0);
    assert!(cache.get(&"err_key".to_string()).is_none());

    let lease = cache
        .get_or_try_insert_with(
            "err_key".to_string(),
            |v| v.len(),
            || Ok("now_ok".to_string()),
        )
        .unwrap();
    assert_eq!(&*lease, "now_ok");
    assert_eq!(cache.len(), 1);
}

#[test]
fn reentrant_initialization_is_rejected() {
    let cache = CalculationCache::<String, String>::new(1024);

    let res = cache.get_or_try_insert_with(
        "outer".to_string(),
        |v| v.len(),
        || {
            let nested = cache.get_or_try_insert_with(
                "inner".to_string(),
                |v| v.len(),
                || Ok("nested".to_string()),
            );
            assert!(nested.is_err(), "reentrant initialization must fail");
            Ok("outer_ok".to_string())
        },
    );

    assert!(res.is_ok());
    assert_eq!(&*res.unwrap(), "outer_ok");
}
