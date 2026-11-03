use xlfn_macros::excel_function;

#[excel_function(name = "A1")]
fn a1_reference() -> f64 {
    0.0
}

#[excel_function(name = "xfd1048576")]
fn final_cell() -> f64 {
    0.0
}

#[excel_function(name = "R1C1")]
fn r1c1_reference() -> f64 {
    0.0
}

#[excel_function(name = "C1R1")]
fn reversed_reference() -> f64 {
    0.0
}

#[excel_function(name = "c")]
fn reserved_column() -> f64 {
    0.0
}

#[excel_function(name = "TRUE")]
fn reserved_logical_value() -> f64 {
    0.0
}

#[excel_function(name = "R１C１")]
fn fullwidth_reference() -> f64 {
    0.0
}

#[excel_function(name = "R١C١")]
fn arabic_indic_reference() -> f64 {
    0.0
}

fn main() {}
