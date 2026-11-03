// module: async_udf
fn cannot_mix<'a, E: crate::async_udf::AsyncExecutor>(
    other: &'a crate::async_udf::runtime::AsyncRuntime<E>,
    snapshot: crate::async_udf::runtime::RuntimeCalculationSnapshot<'a, E>,
) {
    let _ = other.reserve_snapshot(snapshot); // error: E0599
}
