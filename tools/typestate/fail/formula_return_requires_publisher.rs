// module: value
use crate::call_return::ReturnContext;
fn invalid() {
    let _ = ReturnContext::<crate::value::FormulaInputMode>::from_state(()); // error: E0308
}
