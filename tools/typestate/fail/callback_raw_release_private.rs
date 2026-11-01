// module: host_callback
fn invalid(mut value: crate::callback_value::ExcelCallbackValue<'_>) {
    let _ = value.release_in_place(); // error: E0624
}
