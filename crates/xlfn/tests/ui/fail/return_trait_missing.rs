use xlfn::prelude::*;

struct NotAReturn;
static __XLFN_RUNTIME: xlfn::__private::Runtime<()> =
    xlfn::__private::Runtime::new();

#[excel_function(name = "FAIL.RETURN")]
fn bad() -> NotAReturn {
    NotAReturn
}

fn main() {}
