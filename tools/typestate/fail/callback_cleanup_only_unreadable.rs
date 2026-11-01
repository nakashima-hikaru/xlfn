// module: host_callback
fn invalid(mut value: crate::callback_value::CleanupOnlyValue<'_>) {
    let _ = value.borrow(); // error: E0599
}
