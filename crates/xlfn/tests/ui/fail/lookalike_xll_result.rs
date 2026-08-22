use xlfn::prelude::*;

struct XllResult;
static __XLFN_RUNTIME: xlfn::__private::MacroRuntime<()> =
    xlfn::__private::MacroRuntime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> XllResult {
    XllResult
}

fn main() {}
