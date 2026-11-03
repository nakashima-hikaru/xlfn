// module: cache
fn invalid<V>(domain: &super::CacheLookupDomain<V>, entries: super::PendingEntries<V>) {
    let _ = super::reclamation::ReclaimEntries { _domain: domain, entries }; // error: E0451
}
