// module: cache
fn invalid<V>(domain: &super::CacheLookupDomain<V>, entries: super::PendingEntries<V>) {
    let _ = super::reclamation::ReclaimEntries { domain, entries }; // error: E0451
}
