use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function]
fn bad(first: MainThreadContext<'_, ()>, second: ThreadSafeContext<'_, ()>) -> f64 {
    let _ = (first, second);
    0.0
}

fn main() {}
