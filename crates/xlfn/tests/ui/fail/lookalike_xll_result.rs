use xlfn::prelude::*;

struct XllResult;
static __XLFN_RUNTIME: xlfn::__private::Runtime<()> =
    xlfn::__private::Runtime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> XllResult {
    XllResult
}

fn main() {}
