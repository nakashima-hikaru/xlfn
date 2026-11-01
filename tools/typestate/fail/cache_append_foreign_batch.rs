// module: cache
fn invalid<V>(domain: &super::CacheLookupDomain<V>, other: &super::CacheLookupDomain<V>) {
    let mut own = domain.quiesce_and_drain();
    let foreign = other.quiesce_and_drain();
    own.extend(foreign); // error: E0624
}
