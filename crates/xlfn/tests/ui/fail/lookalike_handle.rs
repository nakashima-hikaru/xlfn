use xlfn::prelude::*;

struct Handle;
static __XLFN_RUNTIME: xlfn::macro_support::MacroRuntime<()> =
    xlfn::macro_support::MacroRuntime::new();

#[excel_function(name = "FAIL.LOOKALIKE")]
fn bad() -> Handle {
    Handle
}

fn main() {}
