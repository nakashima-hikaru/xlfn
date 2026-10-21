use xlfn_macros::excel_function;

#[excel_function]
async fn example(#[excel_arg(strict)] value: HandleLease<'_, Dataset>) -> f64 {
    0.0
}

fn main() {}
