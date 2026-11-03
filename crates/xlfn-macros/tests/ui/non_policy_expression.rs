use xlfn_macros::excel_function;

#[excel_function]
fn non_policy_expression(#[excel_arg(missing = policy())] x: f64) -> f64 {
    x
}

fn main() {}
