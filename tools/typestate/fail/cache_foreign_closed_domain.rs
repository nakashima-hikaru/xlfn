// module: cache
fn invalid<V>(domain: &super::CacheLookupDomain<V>, other: &super::CacheLookupDomain<V>) {
    let closed = other.seal();
    let _ = domain.drain_all(closed); // error: E0599
}
