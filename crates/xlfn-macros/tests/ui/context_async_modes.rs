use xlfn_macros::excel_function;

#[excel_function]
async fn main_context(context: MainThreadContext<'_, State>) -> f64 {
    1.0
}

#[excel_function]
async fn thread_context(context: ThreadSafeContext<'_, State>) -> f64 {
    1.0
}

#[excel_function]
async fn macro_context(context: MacroSheetContext<'_, State>) -> f64 {
    1.0
}

#[excel_function]
fn synchronous(context: AsyncContext<'_, State>) -> f64 {
    1.0
}

#[excel_function(thread_safe)]
async fn thread_flag(input: f64) -> f64 {
    input
}

#[excel_function(macro_sheet)]
async fn macro_flag(input: f64) -> f64 {
    input
}

fn main() {}
