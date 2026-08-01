use xlfn_macros::excel_function;

struct MainThreadContext<'a, T>(&'a T);
struct State;

#[excel_function(name = "BAD.MAIN", thread_safe)]
fn bad(#[excel_context(main_thread)] context: &MainThreadContext<'_, State>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
