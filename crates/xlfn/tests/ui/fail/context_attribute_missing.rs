use xlfn::prelude::*;

struct State;
static __XLFN_RUNTIME: xlfn::__private::MacroRuntime<State> =
    xlfn::__private::MacroRuntime::new();

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(context: MainThreadContext<'_, '_, State>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
