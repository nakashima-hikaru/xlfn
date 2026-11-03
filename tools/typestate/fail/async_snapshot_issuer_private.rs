// module: async_udf
fn cannot_retarget<'a, E: crate::async_udf::AsyncExecutor>(
    mut snapshot: crate::async_udf::runtime::RuntimeCalculationSnapshot<'a, E>,
    other: &'a crate::async_udf::runtime::AsyncRuntime<E>,
) {
    snapshot.runtime = other; // error: E0616
}
