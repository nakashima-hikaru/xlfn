use xlfn::prelude::*;

struct XllResult;
static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> XllResult {
    XllResult
}

fn main() {}
