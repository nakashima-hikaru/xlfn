use xlfn_macros::excel_function;

#[excel_function(overwrite = "deny")]
fn value() -> f64 {
    0.0
}

fn main() {}
