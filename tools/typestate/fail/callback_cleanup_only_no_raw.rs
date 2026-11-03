// module: host_callback
fn invalid(value: crate::callback_value::CleanupOnlyValue<'_>) {
    let _ = value.raw(); // error: E0599
}
