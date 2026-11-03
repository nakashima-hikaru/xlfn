use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

use xlfn::ThreadSafeContext as WorkerContext;

#[excel_function]
fn bad(context: WorkerContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
