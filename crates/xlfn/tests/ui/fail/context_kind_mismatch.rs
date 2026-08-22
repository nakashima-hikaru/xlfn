use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::MacroRuntime<()> =
    xlfn::__private::MacroRuntime::new();

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(
    #[excel_context(main_thread)] context: ThreadSafeContext<'_, ()>,
) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
