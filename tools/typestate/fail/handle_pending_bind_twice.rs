// module: handle
fn invalid<T: super::ExcelHandleObject>(pending: super::PendingHandleLease<T>, scope: crate::async_udf::AsyncTaskScope<'_>) {
    let _first = pending.bind(scope);
    let _second = pending.bind(scope); // error: E0382
}
