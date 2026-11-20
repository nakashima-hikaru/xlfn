use xlfn_macros::excel_function;

#[excel_function]
fn typed<T>(value: T) -> f64 { 0.0 }

#[excel_function]
fn constant<const N: usize>() -> f64 { N as f64 }

#[excel_function]
fn bounded<'call: 'static>(value: &'call str) -> f64 { 0.0 }

#[excel_function]
fn where_bound<'call>(value: &'call str) -> f64 where 'call: 'static { 0.0 }

fn main() {}
