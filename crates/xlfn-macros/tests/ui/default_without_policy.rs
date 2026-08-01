use xlfn_macros::excel_function;

#[excel_function]
fn default_without_policy(#[excel_arg(default = 1.0)] x: f64) -> f64 {
    x
}

fn main() {}
