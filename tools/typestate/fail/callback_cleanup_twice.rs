// module: host_callback
fn invalid(value: crate::callback_value::CallbackResult<'_>) {
    let _ = value.release();
    let _ = value.release(); // error: E0382
}
