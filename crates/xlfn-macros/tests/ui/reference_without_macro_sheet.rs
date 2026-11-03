use xlfn_macros::excel_function;

#[excel_function(name = "BAD.REFERENCE")]
fn bad(#[excel_arg(reference)] value: f64) -> f64 {
    value
}

fn main() {}
