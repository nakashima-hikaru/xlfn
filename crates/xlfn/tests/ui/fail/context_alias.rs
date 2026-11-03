use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

type MainContext<'call> = MainThreadContext<'call, ()>;

#[excel_function]
fn bad(context: MainContext<'_>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
