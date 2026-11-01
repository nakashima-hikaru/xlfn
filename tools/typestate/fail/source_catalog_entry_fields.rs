// module: subscription/source
fn invalid<E: super::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    mut entry: crate::source_catalog::CatalogEntry<'_, E, C>,
    other: &C,
) {
    entry.index = usize::MAX; // error: E0616
    entry.catalog = other; // error: E0616
}
