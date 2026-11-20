use xlfn_macros::excel_function;

#[excel_function]
async fn inferred(#[excel_arg(native)] value: &str) -> usize {
    value.len()
}

#[excel_function]
async fn explicit(#[excel_arg(native = str)] value: &str) -> usize {
    value.len()
}

fn main() {}
