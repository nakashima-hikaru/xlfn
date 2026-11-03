use xlfn_macros::excel_function;

#[excel_function()]
fn example(#[excel_arg(strict, strict)] value: f64) -> f64 {
    0.0
}

fn main() {}
