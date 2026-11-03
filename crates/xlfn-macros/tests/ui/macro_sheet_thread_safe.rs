use xlfn_macros::excel_function;

struct MacroSheetContext<'a, T>(&'a T);
struct State;

#[excel_function(name = "BAD.MACRO", thread_safe)]
fn bad(context: MacroSheetContext<'_, State>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
