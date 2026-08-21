use xlfn::prelude::*;

struct NotAReturn;
static __XLFN_RUNTIME: xlfn::macro_support::MacroRuntime<()> =
    xlfn::macro_support::MacroRuntime::new();

#[excel_function(name = "FAIL.RETURN")]
fn bad() -> NotAReturn {
    NotAReturn
}

fn main() {}
