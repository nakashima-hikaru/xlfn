use xlfn::prelude::*;

struct NotAReturn;
static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.RETURN")]
fn bad() -> NotAReturn {
    NotAReturn
}

fn main() {}
