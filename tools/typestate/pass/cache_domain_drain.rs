// module: cache
fn ordered<V>(domain: &super::CacheLookupDomain<V>) {
    let retired = domain.quiesce_and_drain();
    super::reclamation::reclaim_cache_entries(retired);
    let closed = domain.seal();
    // The owning close proof retains its issuing domain across other cleanup.
    let retired = closed.drain();
    super::reclamation::reclaim_cache_entries(retired);
}
