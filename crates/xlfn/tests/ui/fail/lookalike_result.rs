use xlfn::prelude::*;

struct Result;
static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> Result {
    Result
}

fn main() {}
