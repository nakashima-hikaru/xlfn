use xlfn_macros::excel_function;

#[excel_function(return_abi = "native")]
fn invalid() -> f64 {
    0.0
}

fn main() {}
