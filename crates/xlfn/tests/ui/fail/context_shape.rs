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
fn associated_type(context: <() as ContextTypes>::MainThreadContext) -> f64 {
    let _ = context;
    0.0
}

#[excel_function]
fn context_not_first(value: f64, context: MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    value
}

#[excel_function]
fn multiple_contexts(first: MainThreadContext<'_, ()>, second: ThreadSafeContext<'_, ()>) -> f64 {
    let _ = (first, second);
    0.0
}

#[excel_function]
fn context_with_excel_arg(#[excel_arg(name = "context")] context: MainThreadContext<'_, ()>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
