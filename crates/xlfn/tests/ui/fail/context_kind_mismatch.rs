use xlfn::prelude::*;

struct State;
static __XLFN_RUNTIME: xlfn::__private::Runtime<State> =
    xlfn::__private::Runtime::new();

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(
    #[excel_context(main_thread)] context: ThreadSafeContext<'_, State>,
) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
