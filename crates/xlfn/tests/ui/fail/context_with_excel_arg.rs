use xlfn::prelude::*;

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(
    #[excel_context(main_thread)]
    #[excel_arg(name = "bad")]
    context: f64,
) -> f64 {
    context
}

fn main() {}
