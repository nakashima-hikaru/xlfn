// module: value
fn record_after_finish(mut arguments: crate::value::input::ArgumentContext<'_, '_, crate::value::PlainInputMode>) {
    let _ = arguments.finish();
    let _ = arguments.record_decoded(0, "number", &1.0_f64); // error: E0382
}
