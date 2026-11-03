// module: value
fn decode_after_finish<'call>(
    mut arguments: crate::value::input::ArgumentContext<'_, 'call, crate::value::PlainInputMode>,
    value: crate::value::XlValueRef<'call>,
) {
    let _ = arguments.finish();
    let _ = arguments.decode::<f64>(0, "number", value); // error: E0382
}
