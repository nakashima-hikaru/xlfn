// module: host_callback
fn invalid(mut value: crate::callback_value::ExcelCallbackValue<'_>) {
    let _ = value.release();
    let _ = value.borrow(); // error: E0382
}
