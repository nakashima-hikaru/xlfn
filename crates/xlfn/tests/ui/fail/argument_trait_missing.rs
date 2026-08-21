use xlfn::prelude::*;

struct NotAnArgument;
static __XLFN_RUNTIME: xlfn::macro_support::MacroRuntime<()> =
    xlfn::macro_support::MacroRuntime::new();

#[excel_function(name = "FAIL.ARGUMENT")]
fn bad(value: NotAnArgument) -> f64 {
    let _ = value;
    0.0
}

fn main() {}
