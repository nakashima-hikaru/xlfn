// module: value
fn cannot_rewrite_identity(
    finished: crate::value::input::FinishedArguments<'_, crate::value::FormulaInputMode>,
) {
    finished.fingerprint()[0] = 0; // error: E0594
}
