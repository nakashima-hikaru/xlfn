// module: value
fn finished_has_no_recorder<'call>(
    mut finished: crate::value::input::FinishedArguments<'call, crate::value::PlainInputMode>,
    value: crate::value::XlValueRef<'call>,
) {
    let _ = finished.decode::<f64>(0, "number", value); // error: E0599
}
