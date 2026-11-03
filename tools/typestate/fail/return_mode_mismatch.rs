// module: value
use crate::call_return::ReturnContext;
fn formula(_: &mut ReturnContext<'_, '_, crate::value::FormulaInputMode>) {}
fn invalid(plain: &mut ReturnContext<'_, '_>) {
    formula(plain); // error: E0308
}
