use xlfn_macros::excel_function;

#[excel_function(return_abi = "fp12", return_abi = "xloper12")]
fn invalid() -> f64 {
    0.0
}

fn main() {}
