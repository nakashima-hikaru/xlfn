// module: value
fn finish_twice(arguments: crate::value::input::ArgumentContext<'_, crate::value::PlainInputMode>) {
    let _ = arguments.finish();
    let _ = arguments.finish(); // error: E0382
}
