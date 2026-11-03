// module: value
fn cannot_extract_mut_context(finished: &mut crate::value::input::FinishedArguments<'_, crate::value::PlainInputMode>) {
    let _ = &mut finished.call; // error: E0616
}
