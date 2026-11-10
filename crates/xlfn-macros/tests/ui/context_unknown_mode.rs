use xlfn_macros::excel_function;

#[excel_function]
fn bad(#[excel_context(unknown)] context: MainThreadContext<'_, State>) -> f64 {
    1.0
}

fn main() {}
