use xlfn::prelude::*;

struct Result;
static __XLFN_RUNTIME: xlfn::__private::Runtime<()> =
    xlfn::__private::Runtime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> Result {
    Result
}

fn main() {}
