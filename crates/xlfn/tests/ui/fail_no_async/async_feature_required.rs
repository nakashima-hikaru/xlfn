use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::MacroRuntime<()> =
    xlfn::__private::MacroRuntime::new();

#[excel_function(name = "FAIL.ASYNC.FEATURE")]
async fn bad() -> f64 {
    0.0
}

fn main() {}
