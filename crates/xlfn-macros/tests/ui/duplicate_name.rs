use xlfn_macros::excel_function;

#[excel_function(name = "ONE", name = "TWO")]
fn bad() -> f64 {
    0.0
}

fn main() {}
