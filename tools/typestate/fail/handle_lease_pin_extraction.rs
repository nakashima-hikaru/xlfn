// module: handle
fn invalid<T: super::ExcelHandleObject>(lease: super::HandleLease<'_, T>) {
    drop(lease._lease); // error: E0616
}
