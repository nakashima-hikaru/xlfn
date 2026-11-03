use xlfn_macros::excel_function;

#[excel_function(macro_sheet)]
fn example(#[excel_arg(strict, reference)] value: ExcelReference<'_>) -> f64 {
    0.0
}

fn main() {}
