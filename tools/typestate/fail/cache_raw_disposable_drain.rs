// module: cache
fn invalid<V>(domain: &super::CacheLookupDomain<V>, other: &super::CacheLookupDomain<V>) {
    if let Some(Ok(batch)) = other.domain.poll_quiesce() {
        let _ = domain.drain_generation(batch); // error: E0599
    }
}
