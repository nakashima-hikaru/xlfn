use xlfn_macros::excel_function;

#[excel_function(name = "BAD.CATEGORY", category = "A", category = "B")]
fn bad() -> f64 {
    0.0
}

fn main() {}
