use xlfn_macros::excel_function;

#[excel_function(async)]
async fn value() -> f64 {
    0.0
}

fn main() {}
