use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(context: MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
