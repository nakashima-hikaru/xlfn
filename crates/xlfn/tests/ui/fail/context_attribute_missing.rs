use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::macro_support::MacroRuntime<()> =
    xlfn::macro_support::MacroRuntime::new();

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(context: MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
