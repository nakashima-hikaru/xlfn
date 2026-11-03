use xlfn_macros::excel_function;

#[excel_function]
fn later(input: f64, context: MainThreadContext<'_, State>) -> f64 {
    input
}

#[excel_function]
fn multiple(context: MainThreadContext<'_, State>, other: ThreadSafeContext<'_, State>) -> f64 {
    1.0
}

#[excel_function]
fn borrowed(context: &MainThreadContext<'_, State>) -> f64 {
    1.0
}

#[excel_function]
fn associated(context: <Owner as Trait>::MainThreadContext<'_, State>) -> f64 {
    1.0
}

#[excel_function]
fn missing_addin(context: MainThreadContext<'_>) -> f64 {
    1.0
}

#[excel_function]
fn excel_argument(#[excel_arg(name = "context")] context: MainThreadContext<'_, State>) -> f64 {
    1.0
}

fn main() {}
