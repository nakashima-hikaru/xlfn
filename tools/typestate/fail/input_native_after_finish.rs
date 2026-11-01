// module: value
fn native_after_finish(mut arguments: crate::value::input::ArgumentContext<'_, crate::value::PlainInputMode>) {
    let _ = arguments.finish();
    let _ = arguments.native_f64(0, "number", 1.0); // error: E0382
}
