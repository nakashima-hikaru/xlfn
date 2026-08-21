use xlfn::prelude::*;

struct Result;
static __XLFN_RUNTIME: xlfn::macro_support::MacroRuntime<()> =
    xlfn::macro_support::MacroRuntime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> Result {
    Result
}

fn main() {}
