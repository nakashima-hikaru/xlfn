// module: cache
fn invalid<V>(domain: &super::CacheLookupDomain<V>, other: &super::CacheLookupDomain<V>) {
    let own = domain.quiesce_and_drain();
    let foreign = other.quiesce_and_drain();
    let _ = super::reclamation::merge_reclaims(domain, [own, foreign]); // error: E0425
}
