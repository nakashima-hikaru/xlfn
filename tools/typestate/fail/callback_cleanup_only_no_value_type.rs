// module: host_callback
fn invalid(value: crate::callback_value::CleanupOnlyValue<'_>) {
    let _ = value.value_type(); // error: E0599
}
