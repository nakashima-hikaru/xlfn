//! Composable input conversions for custom [`FromExcel`](crate::value::FromExcel) implementations.
//!
//! These functions apply the same presence, shape, and allocation limits as
//! built-in worksheet parameters. The supplied converter handles one cell;
//! pass a type's `FromExcel::from_excel` method or another conversion function.
//! Any borrowed result remains tied to the input's call lifetime.

use super::{
    BoundedVarArgs, Column, GridView, Matrix, OptionalExcelValue, Row, XlValueRef, XlValueType,
    convert_owned_grid_elements,
};
use crate::error::{InputError, Shape};
use crate::{XllError, XllResult};

pub(super) enum GridShape {
    Matrix,
    Vector,
    Row,
    Column,
}

pub(super) fn grid<'call>(
    value: XlValueRef<'call>,
    argument: &'static str,
    shape: GridShape,
) -> XllResult<GridView<'call>> {
    let grid = GridView::from_value(value, argument)?;
    let (rows, columns) = grid.shape();
    let expected = match shape {
        GridShape::Matrix => None,
        GridShape::Vector if rows != 1 && columns != 1 => Some(Shape {
            rows: 1,
            columns: rows * columns,
        }),
        GridShape::Row if rows != 1 => Some(Shape { rows: 1, columns }),
        GridShape::Column if columns != 1 => Some(Shape { rows, columns: 1 }),
        _ => None,
    };
    if let Some(expected) = expected {
        return Err(XllError::Shape {
            expected,
            actual: Shape { rows, columns },
        });
    }
    Ok(grid)
}

/// Converts a scalar or rectangular array into an owned matrix.
///
/// ```
/// use xlfn::value::{convert, FromExcel, Matrix, XlValueRef};
/// use xlfn::XllResult;
///
/// struct NumericMatrix(Matrix<f64>);
///
/// impl<'call> FromExcel<'call> for NumericMatrix {
///     fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
///         convert::matrix(value, argument, f64::from_excel).map(Self)
///     }
/// }
/// ```
pub fn matrix<'call, T>(
    value: XlValueRef<'call>,
    argument: &'static str,
    mut convert: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<T>,
) -> XllResult<Matrix<T>> {
    let grid = grid(value, argument, GridShape::Matrix)?;
    let (rows, columns) = grid.shape();
    let data = convert_owned_grid_elements(&grid, argument, |cell| convert(cell, argument))?;
    Matrix::new(rows, columns, data)
}

/// Converts a scalar, one row, or one column into an owned vector.
pub fn vector<'call, T>(
    value: XlValueRef<'call>,
    argument: &'static str,
    mut convert: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<T>,
) -> XllResult<Vec<T>> {
    let grid = grid(value, argument, GridShape::Vector)?;
    convert_owned_grid_elements(&grid, argument, |cell| convert(cell, argument))
}

/// Converts a scalar or one row into an owned row, rejecting multiple rows.
pub fn row<'call, T>(
    value: XlValueRef<'call>,
    argument: &'static str,
    mut convert: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<T>,
) -> XllResult<Row<T>> {
    let grid = grid(value, argument, GridShape::Row)?;
    convert_owned_grid_elements(&grid, argument, |cell| convert(cell, argument)).map(Row)
}

/// Converts a scalar or one column into an owned column, rejecting multiple columns.
pub fn column<'call, T>(
    value: XlValueRef<'call>,
    argument: &'static str,
    mut convert: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<T>,
) -> XllResult<Column<T>> {
    let grid = grid(value, argument, GridShape::Column)?;
    convert_owned_grid_elements(&grid, argument, |cell| convert(cell, argument)).map(Column)
}

/// Converts a scalar or vector after checking its maximum element count.
///
/// The converter is never called if `MAX` is zero or the input exceeds it.
pub fn bounded_var_args<'call, T, const MAX: usize>(
    value: XlValueRef<'call>,
    argument: &'static str,
    mut convert: impl FnMut(XlValueRef<'call>, &'static str) -> XllResult<T>,
) -> XllResult<BoundedVarArgs<T, MAX>> {
    let grid = bounded_grid::<MAX>(value, argument)?;
    let data = convert_owned_grid_elements(&grid, argument, |cell| convert(cell, argument))?;
    Ok(BoundedVarArgs(data))
}

pub(super) fn bounded_grid<'call, const MAX: usize>(
    value: XlValueRef<'call>,
    argument: &'static str,
) -> XllResult<GridView<'call>> {
    if MAX == 0 {
        return Err(XllError::input(
            argument,
            InputError::Malformed("bounded varargs maximum must be non-zero"),
        ));
    }
    let grid = grid(value, argument, GridShape::Vector)?;
    let actual = grid.cells().len();
    if actual > MAX {
        return Err(XllError::input(
            argument,
            InputError::TooLarge { limit: MAX, actual },
        ));
    }
    Ok(grid)
}

/// Maps an omitted or blank input to `None`, converting only a present value.
///
/// The converter may itself decode a collection, for example:
/// `optional(value, argument, |value, argument| matrix(value, argument, f64::from_excel))`.
pub fn optional<'call, T>(
    value: XlValueRef<'call>,
    argument: &'static str,
    convert: impl FnOnce(XlValueRef<'call>, &'static str) -> XllResult<T>,
) -> XllResult<Option<T>> {
    match value.value_type() {
        XlValueType::Missing | XlValueType::Nil => Ok(None),
        _ => convert(value, argument).map(Some),
    }
}

/// Preserves the distinction between omitted, blank, and present inputs.
pub fn optional_value<'call, T>(
    value: XlValueRef<'call>,
    argument: &'static str,
    convert: impl FnOnce(XlValueRef<'call>, &'static str) -> XllResult<T>,
) -> XllResult<OptionalExcelValue<T>> {
    match value.value_type() {
        XlValueType::Missing => Ok(OptionalExcelValue::Missing),
        XlValueType::Nil => Ok(OptionalExcelValue::Blank),
        _ => convert(value, argument).map(OptionalExcelValue::Value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{CallContext, ExcelParameter, FromExcel, MAX_ARRAY_BYTES, PlainInputMode};
    use std::fmt::Debug;
    use xlfn_sys::{XLOPER12, XLOPER12Array, XLOPER12Value, XLTYPE_MULTI};

    fn compare<T>(raw: &XLOPER12, convert: impl FnOnce(XlValueRef<'_>) -> XllResult<T>)
    where
        T: for<'call> ExcelParameter<'call, PlainInputMode> + PartialEq + Debug,
    {
        crate::call::with_excel_call_scope_and_state(raw, |raw, scope| {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            let actual = convert(value);
            let expected = T::decode(value, "values", &CallContext::plain(scope), &mut ());
            match (actual, expected) {
                (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
                (Err(actual), Err(expected)) => {
                    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
                }
                result => panic!("public and built-in conversion differ: {result:?}"),
            }
        });
    }

    fn compare_collections(raw: &XLOPER12) {
        compare(raw, |value| matrix(value, "values", f64::from_excel));
        compare(raw, |value| vector(value, "values", f64::from_excel));
        compare(raw, |value| row(value, "values", f64::from_excel));
        compare(raw, |value| column(value, "values", f64::from_excel));
        compare(raw, |value| {
            bounded_var_args::<_, 2>(value, "values", f64::from_excel)
        });
        compare(raw, |value| {
            optional(value, "values", |value, argument| {
                matrix(value, argument, f64::from_excel)
            })
        });
        compare(raw, |value| {
            optional_value(value, "values", |value, argument| {
                vector(value, argument, f64::from_excel)
            })
        });
    }

    #[test]
    fn public_collection_conversions_match_builtin_values_and_errors() {
        for raw in [
            XLOPER12::number(3.0),
            XLOPER12::number(f64::NAN),
            XLOPER12::nil(),
            XLOPER12::missing(),
        ] {
            compare_collections(&raw);
        }
        let mut cells = [
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
            XLOPER12::number(4.0),
        ];
        for (rows, columns) in [(1, 4), (4, 1), (2, 2), (1, 2)] {
            let raw = XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        rows,
                        columns,
                        values: cells.as_mut_ptr(),
                    },
                },
                xltype: XLTYPE_MULTI,
            };
            compare_collections(&raw);
        }
    }

    #[test]
    fn public_presence_conversions_do_not_convert_absent_values() {
        for raw in [XLOPER12::missing(), XLOPER12::nil()] {
            let value = XlValueRef::from_array_cell(&raw).unwrap();
            assert_eq!(
                optional::<()>(value, "values", |_, _| panic!("absent input")).unwrap(),
                None
            );
            let result =
                optional_value::<()>(value, "values", |_, _| panic!("absent input")).unwrap();
            assert!(matches!(
                result,
                OptionalExcelValue::Missing | OptionalExcelValue::Blank
            ));
        }
    }

    #[test]
    fn public_collection_limits_apply_before_element_conversion() {
        const ELEMENT_BYTES: usize = 4096;
        let mut cells = vec![XLOPER12::number(1.0); MAX_ARRAY_BYTES / ELEMENT_BYTES + 1];
        let raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    rows: cells.len() as i32,
                    columns: 1,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let value = XlValueRef::from_array_cell(&raw).unwrap();
        let error = matrix::<[u8; ELEMENT_BYTES]>(value, "values", |_, _| {
            panic!("allocation budget must be checked first")
        })
        .unwrap_err();
        assert!(matches!(
            error,
            XllError::Input {
                argument: "values",
                reason: InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    ..
                }
            }
        ));
        let error = bounded_var_args::<(), 0>(value, "values", |_, _| {
            panic!("element limit must be checked first")
        })
        .unwrap_err();
        assert!(matches!(
            error,
            XllError::Input {
                argument: "values",
                reason: InputError::Malformed(_)
            }
        ));
    }

    #[test]
    fn custom_collection_identity_matches_the_converted_value() {
        use crate::input_identity::{InputFingerprint, InputFingerprintBuilder};
        use crate::value::{ExcelInputIdentity, FormulaInputMode, InputIdentityEncoder};

        struct OptionalMatrix(Option<Matrix<f64>>);

        impl<'call> FromExcel<'call> for OptionalMatrix {
            fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
                optional(value, argument, |value, argument| {
                    matrix(value, argument, f64::from_excel)
                })
                .map(Self)
            }
        }

        impl ExcelInputIdentity for OptionalMatrix {
            fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
                encoder.bool(self.0.is_some());
                if let Some(matrix) = &self.0 {
                    encoder.u64(matrix.rows() as u64);
                    encoder.u64(matrix.columns() as u64);
                    for value in matrix.iter() {
                        encoder.f64(*value);
                    }
                }
            }
        }

        fn fingerprint<T: for<'call> ExcelParameter<'call, FormulaInputMode>>(
            raw: &XLOPER12,
        ) -> InputFingerprint {
            crate::call::with_excel_call_scope_and_state(raw, |raw, scope| {
                let mut fingerprint = InputFingerprintBuilder::new(1);
                fingerprint
                    .with_argument(0, "values", |identity| {
                        T::decode(
                            XlValueRef::from_array_cell(raw)?,
                            "values",
                            &CallContext::plain(scope),
                            identity,
                        )
                    })
                    .unwrap();
                fingerprint.finish().unwrap()
            })
        }

        for raw in [XLOPER12::number(-0.0), XLOPER12::missing(), XLOPER12::nil()] {
            assert_eq!(
                fingerprint::<OptionalMatrix>(&raw),
                fingerprint::<Option<Matrix<f64>>>(&raw)
            );
        }
        let mut cells = [XLOPER12::number(1.0), XLOPER12::number(2.0)];
        let raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    rows: 1,
                    columns: 2,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: XLTYPE_MULTI,
        };
        assert_eq!(
            fingerprint::<OptionalMatrix>(&raw),
            fingerprint::<Option<Matrix<f64>>>(&raw)
        );
    }
}
