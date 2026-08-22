use xlfn::prelude::*;

struct Handle;
static __XLFN_RUNTIME: xlfn::__private::MacroRuntime<()> =
    xlfn::__private::MacroRuntime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> Handle {
    Handle
}

fn main() {}
