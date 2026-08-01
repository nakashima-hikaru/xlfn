use xlfn_macros::excel_function;

#[excel_function(visibility = "hidden")]
fn value() -> f64 {
    0.0
}

fn main() {}
