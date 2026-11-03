use xlfn_macros::excel_function;

#[excel_function]
fn unused_default(#[excel_arg(default = 1.0, blank = error, missing = error)] x: f64) -> f64 {
    x
}

fn main() {}
