use xlfn_macros::excel_function;

#[excel_function(thread_safe)]
fn thread_context(context: ThreadSafeContext<'_, State>) -> f64 {
    1.0
}

#[excel_function(macro_sheet)]
fn macro_context(context: MacroSheetContext<'_, State>) -> f64 {
    1.0
}

fn main() {}
