use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::macro_support::MacroRuntime<()> =
    xlfn::macro_support::MacroRuntime::new();

#[excel_function(name = "FAIL.ASYNC.FEATURE")]
async fn bad() -> f64 {
    0.0
}

fn main() {}
