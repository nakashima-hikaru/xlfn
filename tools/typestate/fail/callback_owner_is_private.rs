// module: host_callback
fn invalid(value: &crate::callback_value::ExcelCallbackValue<'_>) {
    let _ = &value.owner; // error: E0616
}
