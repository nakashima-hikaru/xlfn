use xlfn::prelude::*;

#[excel_function(name = "FAIL.CONTEXT")]
fn bad(#[excel_context] context: f64) -> f64 {
    context
}

fn main() {}
