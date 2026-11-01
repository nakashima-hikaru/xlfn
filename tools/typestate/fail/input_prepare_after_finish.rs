// module: value
fn prepare_after_finish<'call>(
    mut arguments: crate::value::input::ArgumentContext<'call, crate::value::PlainInputMode>,
    value: crate::value::XlValueRef<'call>,
) {
    let _ = arguments.finish();
    let _ = arguments.prepare::<f64>(0, "number", value); // error: E0382
}
