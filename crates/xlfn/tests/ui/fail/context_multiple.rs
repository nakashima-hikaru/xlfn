use xlfn::prelude::*;

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(
    #[excel_context(main_thread)] first: f64,
    #[excel_context(thread_safe)] second: f64,
) -> f64 {
    first + second
}

fn main() {}
