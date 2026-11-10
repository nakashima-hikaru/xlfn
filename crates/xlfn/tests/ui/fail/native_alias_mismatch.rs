use xlfn::prelude::*;
static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();
type Price = String;
#[excel_function]
fn bad(#[excel_arg(native = f64)] value: Price) -> String { value }
fn main() {}
