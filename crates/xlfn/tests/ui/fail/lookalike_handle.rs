use xlfn::prelude::*;

struct Handle;
static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> Handle {
    Handle
}

fn main() {}
