use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function]
fn bad(value: f64, context: MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    value
}

fn main() {}
