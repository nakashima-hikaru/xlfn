// module: value
fn cannot_extract_mut_context(arguments: &mut crate::value::input::ArgumentContext<'_, '_, crate::value::PlainInputMode>) {
    let _ = &mut arguments.call; // error: E0616
}
