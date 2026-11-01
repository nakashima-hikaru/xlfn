// module: value
fn cannot_replace_fingerprint(mut arguments: crate::value::input::ArgumentContext<'_, crate::value::PlainInputMode>) {
    arguments.inputs = (); // error: E0616
}
