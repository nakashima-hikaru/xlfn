use xlfn_macros::excel_function;

#[excel_function]
fn consume(#[excel_arg(handle)] value: String) -> f64 {
    let _ = value;
    0.0
}

fn main() {}
