use xlfn_macros::excel_function;

#[excel_function(return_abi = 123)]
fn invalid() -> f64 {
    0.0
}

fn main() {}
