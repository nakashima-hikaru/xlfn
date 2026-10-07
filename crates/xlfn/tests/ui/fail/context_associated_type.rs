use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

trait ContextTypes {
    type MainThreadContext;
}

impl ContextTypes for () {
    type MainThreadContext = xlfn::MainThreadContext<'static, ()>;
}

#[excel_function]
fn bad(context: <() as ContextTypes>::MainThreadContext) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
