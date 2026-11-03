// module: async_udf
fn owner_bound<'a, E: crate::async_udf::AsyncExecutor>(
    snapshot: crate::async_udf::runtime::RuntimeCalculationSnapshot<'a, E>,
) -> crate::XllResult<()> {
    snapshot.preflight()?;
    drop(snapshot.reserve()?);
    Ok(())
}
