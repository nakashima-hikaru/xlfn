use xlfn::prelude::*;

struct NotAReturn;
static __XLFN_RUNTIME: xlfn::__private::MacroRuntime<()> =
    xlfn::__private::MacroRuntime::new();

#[excel_function(name = "FAIL.RETURN")]
fn bad() -> NotAReturn {
    NotAReturn
}

fn main() {}
