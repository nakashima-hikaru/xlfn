use xlfn_macros::excel_function;

#[excel_function()]
fn example(#[excel_arg(strict)] value: NumericMatrixRef<'_>) -> f64 {
    0.0
}

fn main() {}
