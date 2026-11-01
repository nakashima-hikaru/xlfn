// module: handle
fn invalid<T: super::ExcelHandleObject>(pending: super::PendingHandleLease<T>) {
    drop(pending.lease); // error: E0616
}
