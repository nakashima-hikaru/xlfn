// module: value
use crate::call_return::ReturnContext;
use crate::value::FormulaInputMode;
fn invalid(context: ReturnContext<'_, '_, FormulaInputMode, (), ()>) {
    let _: ReturnContext<'_, '_, FormulaInputMode> = context; // error: E0308
}
