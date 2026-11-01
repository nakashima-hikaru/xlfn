// module: host_callback
fn ordered(mut value: crate::callback_value::CallbackResult<'_>) -> crate::XllResult<()> {
    if let Ok(readable) = value.readable() { let _ = readable.borrow()?; }
    value.release()
}
