// module: handle
fn invalid<'generation, T: super::ExcelHandleObject>(pending: super::PendingHandleLease<T>, scope: crate::async_udf::AsyncTaskScope<'generation>) -> super::HandleLease<'generation, T> {
    pending.bind(scope) // error: E0308
}
