use xlfn_macros::excel_function;

#[excel_function(name = "bad name")]
fn spaces() -> f64 {
    0.0
}

#[excel_function(name = "bad-name")]
fn hyphen() -> f64 {
    0.0
}

#[excel_function(name = "bad+name")]
fn operator() -> f64 {
    0.0
}

#[excel_function(name = "1value")]
fn leading_digit() -> f64 {
    0.0
}

fn main() {}
