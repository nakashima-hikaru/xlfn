// module: handle
fn checked<'generation, T: super::ExcelHandleObject>(
    pending: super::PendingHandleLease<T>,
    scope: crate::async_udf::AsyncTaskScope<'generation>,
) -> crate::XllResult<super::HandleLease<'generation, T>> {
    pending.bind(scope)
}
