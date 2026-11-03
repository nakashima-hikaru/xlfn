//! Same-body Q/native controls for live Excel ABI qualification.
use xlfn::prelude::*;

#[excel_function(name = "BENCH.ABI.F641.Q", thread_safe)]
pub fn f64_1_q(#[excel_arg(strict)] a: f64) -> f64 {
    a
}

#[excel_function(name = "BENCH.ABI.F641.NATIVE", thread_safe)]
pub fn f64_1_native(a: f64) -> f64 {
    a
}

#[excel_function(name = "BENCH.ABI.F642.Q", thread_safe)]
pub fn f64_2_q(#[excel_arg(strict)] a: f64, #[excel_arg(strict)] b: f64) -> f64 {
    a + b
}

#[excel_function(name = "BENCH.ABI.F642.NATIVE", thread_safe)]
pub fn f64_2_native(a: f64, b: f64) -> f64 {
    a + b
}

#[excel_function(name = "BENCH.ABI.F644.Q", thread_safe)]
pub fn f64_4_q(
    #[excel_arg(strict)] a: f64,
    #[excel_arg(strict)] b: f64,
    #[excel_arg(strict)] c: f64,
    #[excel_arg(strict)] d: f64,
) -> f64 {
    a + b + c + d
}

#[excel_function(name = "BENCH.ABI.F644.NATIVE", thread_safe)]
pub fn f64_4_native(a: f64, b: f64, c: f64, d: f64) -> f64 {
    a + b + c + d
}

#[excel_function(name = "BENCH.ABI.F648.Q", thread_safe)]
#[allow(clippy::too_many_arguments, reason = "Measures eight Excel arguments")]
pub fn f64_8_q(
    #[excel_arg(strict)] a: f64,
    #[excel_arg(strict)] b: f64,
    #[excel_arg(strict)] c: f64,
    #[excel_arg(strict)] d: f64,
    #[excel_arg(strict)] e: f64,
    #[excel_arg(strict)] f: f64,
    #[excel_arg(strict)] g: f64,
    #[excel_arg(strict)] h: f64,
) -> f64 {
    a + b + c + d + e + f + g + h
}

#[excel_function(name = "BENCH.ABI.F648.NATIVE", thread_safe)]
#[allow(clippy::too_many_arguments, reason = "Measures eight Excel arguments")]
pub fn f64_8_native(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64) -> f64 {
    a + b + c + d + e + f + g + h
}

#[excel_function(name = "BENCH.ABI.BOOL.Q", thread_safe)]
pub fn bool_q(#[excel_arg(strict)] value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

#[excel_function(name = "BENCH.ABI.BOOL.NATIVE", thread_safe)]
pub fn bool_native(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

#[excel_function(name = "BENCH.ABI.I32.Q", thread_safe)]
pub fn i32_q(#[excel_arg(strict)] value: i32) -> f64 {
    f64::from(value)
}

#[excel_function(name = "BENCH.ABI.I32.NATIVE", thread_safe)]
pub fn i32_native(value: i32) -> f64 {
    f64::from(value)
}

#[excel_function(name = "BENCH.ABI.STR.Q", thread_safe)]
pub fn str_q(#[excel_arg(strict)] value: String) -> f64 {
    value.chars().count() as f64
}

#[excel_function(name = "BENCH.ABI.STR.NATIVE", thread_safe)]
pub fn str_native(value: String) -> f64 {
    value.chars().count() as f64
}

#[excel_function(name = "BENCH.ABI.MATRIX.Q", thread_safe)]
pub fn matrix_q(values: MatrixRef<'_, f64>) -> f64 {
    values.iter().copied().sum()
}

#[excel_function(name = "BENCH.ABI.MATRIX.NATIVE", thread_safe)]
pub fn matrix_native(values: NumericMatrixRef<'_>) -> f64 {
    values.iter().copied().sum()
}
