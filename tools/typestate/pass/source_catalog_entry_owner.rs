// module: subscription/source
fn ordered<E: super::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    catalog: &C,
) -> crate::XllResult<()> {
    for entry in crate::source_catalog::CatalogEntry::all(catalog) {
        entry.request_stop();
        entry.shutdown_and_wait()?;
    }
    Ok(())
}
