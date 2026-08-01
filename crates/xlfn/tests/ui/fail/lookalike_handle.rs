use xlfn::prelude::*;

struct Handle;
static __XLFN_RUNTIME: xlfn::__private::Runtime<()> =
    xlfn::__private::Runtime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> Handle {
    Handle
}

fn main() {}
