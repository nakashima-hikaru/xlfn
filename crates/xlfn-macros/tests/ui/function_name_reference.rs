use xlfn_macros::excel_function;

#[excel_function(name = "A1")]
fn a1_reference() -> f64 {
    0.0
}


fn main() {}
