//! Call-borrowed numeric arrays supplied through Excel's FP12 ABI.

use std::ops::Index;

use crate::error::InputError;
use crate::{XllError, XllResult};

/// A borrowed row-major numeric matrix supplied directly by Excel.
///
/// `#[excel_function]` registers this argument with Excel's numeric-array
/// (`K%`/FP12) ABI. Excel performs numeric coercion before calling the function;
/// blanks become zero, and inputs that cannot be coerced to a numeric array
/// produce a worksheet error without invoking the Rust function. Cell type,
/// blank, missing, and error distinctions are unavailable through this type.
/// Use [`super::Matrix`] or [`super::MatrixRef`] when those distinctions or
/// xlfn's strict per-cell conversion policies matter.
///
/// The elements borrow Excel's contiguous double buffer: no typed scratch
/// allocation or copy is needed. This type is available only to synchronous
/// functions and cannot outlive the current Excel invocation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericMatrixRef<'call> {
    rows: usize,
    columns: usize,
    values: &'call [f64],
}

impl<'call> NumericMatrixRef<'call> {
    /// Number of rows in the matrix.
    pub const fn rows(self) -> usize {
        self.rows
    }

    /// Number of columns in the matrix.
    pub const fn columns(self) -> usize {
        self.columns
    }

    /// The matrix dimensions as `(rows, columns)`.
    pub const fn shape(self) -> (usize, usize) {
        (self.rows, self.columns)
    }

    /// All elements in row-major order, borrowing the current Excel call.
    pub const fn as_slice(self) -> &'call [f64] {
        self.values
    }

    /// Iterates over all elements in row-major order.
    pub fn iter(self) -> std::slice::Iter<'call, f64> {
        self.values.iter()
    }

    /// Borrows a row, or returns `None` for an out-of-bounds row index.
    pub fn row(self, row: usize) -> Option<&'call [f64]> {
        (row < self.rows).then(|| {
            let start = row * self.columns;
            &self.values[start..start + self.columns]
        })
    }

    /// Looks up one element by its zero-based row and column.
    pub fn get(self, row: usize, column: usize) -> Option<&'call f64> {
        if row >= self.rows || column >= self.columns {
            return None;
        }
        self.values.get(row * self.columns + column)
    }

    /// Borrows a live variable-length FP12 argument after validating its shape.
    ///
    /// # Safety
    ///
    /// A non-null, aligned `raw` must point to a readable FP12 header. Null or
    /// misaligned pointers are rejected without accessing their storage.
    /// If its dimensions pass validation, the header must be followed by
    /// `rows * columns` initialized doubles, all readable for `'call`. Excel
    /// must not mutate this storage during the borrow.
    pub(crate) unsafe fn from_raw(
        raw: *const xlfn_sys::FP12,
        argument: &'static str,
    ) -> XllResult<Self> {
        if raw.is_null() {
            return Err(XllError::input(
                argument,
                InputError::Malformed("null numeric matrix argument"),
            ));
        }
        if !raw.is_aligned() {
            return Err(XllError::input(
                argument,
                InputError::Malformed("misaligned numeric matrix pointer"),
            ));
        }
        // SAFETY: alignment was checked and the caller guarantees a live header.
        let rows = unsafe { (*raw).rows };
        // SAFETY: The second dimension is in the same live FP12 header.
        let columns = unsafe { (*raw).columns };
        let rows = usize::try_from(rows).map_err(|_| {
            XllError::input(
                argument,
                InputError::Malformed("invalid numeric matrix dimensions"),
            )
        })?;
        let columns = usize::try_from(columns).map_err(|_| {
            XllError::input(
                argument,
                InputError::Malformed("invalid numeric matrix dimensions"),
            )
        })?;
        let length = super::matrix::matrix_element_count(rows, columns, argument)?;
        let bytes = length
            .checked_mul(std::mem::size_of::<f64>())
            .ok_or_else(|| {
                XllError::input(
                    argument,
                    InputError::Malformed("invalid numeric matrix dimensions"),
                )
            })?;
        if bytes > super::MAX_ARRAY_BYTES {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: super::MAX_ARRAY_BYTES,
                    actual: bytes,
                },
            ));
        }
        // SAFETY: the FP12 payload starts at its flexible array member; the
        // caller guarantees all validated `length` doubles are readable and
        // remain immutable for 'call. Bounds also fit Rust's slice byte limit.
        let payload = unsafe { std::ptr::addr_of!((*raw).array).cast::<f64>() };
        // SAFETY: The caller supplies the validated contiguous payload at the
        // address above, which remains readable and immutable for 'call.
        let values = unsafe { std::slice::from_raw_parts(payload, length) };
        // Reduce each small block without a data-dependent branch per cell.
        // The bounded blocks retain early rejection on invalid large inputs.
        if values.chunks(64).any(|chunk| {
            !chunk
                .iter()
                .fold(true, |finite, number| finite & number.is_finite())
        }) {
            return Err(XllError::input(argument, InputError::NonFinite));
        }
        Ok(Self {
            rows,
            columns,
            values,
        })
    }
}

impl Index<(usize, usize)> for NumericMatrixRef<'_> {
    type Output = f64;

    fn index(&self, (row, column): (usize, usize)) -> &Self::Output {
        self.get(row, column)
            .expect("numeric matrix index out of bounds")
    }
}

impl<'call> IntoIterator for NumericMatrixRef<'call> {
    type Item = &'call f64;
    type IntoIter = std::slice::Iter<'call, f64>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl AsRef<[f64]> for NumericMatrixRef<'_> {
    fn as_ref(&self) -> &[f64] {
        self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    struct NumericInput<const N: usize> {
        rows: i32,
        columns: i32,
        values: [f64; N],
    }

    #[test]
    fn miri_borrows_flexible_numeric_payload_without_copying() {
        let raw = NumericInput {
            rows: 2,
            columns: 3,
            values: [0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
        };
        // SAFETY: NumericInput has FP12's header and six initialized doubles.
        let matrix =
            unsafe { NumericMatrixRef::from_raw((&raw const raw).cast(), "matrix") }.unwrap();
        assert_eq!(matrix.shape(), (2, 3));
        assert_eq!(matrix.as_slice().as_ptr(), raw.values.as_ptr());
        assert_eq!(matrix.row(1), Some([3.0, 4.0, 5.0].as_slice()));
        assert_eq!(matrix[(1, 2)], 5.0);
        assert_eq!(matrix.get(0, 3), None);
        assert_eq!(matrix.get(2, 0), None);
        assert_eq!(matrix.row(2), None);
        assert_eq!(matrix.into_iter().copied().sum::<f64>(), 15.0);
    }

    #[test]
    fn miri_rejects_invalid_shapes_before_reading_payload() {
        for (rows, columns) in [
            (0, 1),
            (1, 0),
            (-1, 1),
            (1, -1),
            (1_048_577, 1),
            (1, 16_385),
        ] {
            let raw = xlfn_sys::FP12 {
                rows,
                columns,
                array: [0.0],
            };
            // SAFETY: the header is live. Invalid dimensions must be rejected
            // before the declared one-element payload is traversed.
            let result = unsafe { NumericMatrixRef::from_raw(&raw, "observations") };
            assert!(matches!(
                result,
                Err(XllError::Input {
                    argument: "observations",
                    ..
                })
            ));
        }
        // SAFETY: null is detected before any pointer dereference.
        assert!(unsafe { NumericMatrixRef::from_raw(std::ptr::null(), "matrix") }.is_err());
    }

    #[test]
    fn miri_numeric_cell_limit_is_an_input_error_even_when_count_exceeds_u32() {
        let raw = xlfn_sys::FP12 {
            rows: 1_048_576,
            columns: 4096,
            array: [0.0],
        };
        // SAFETY: the live header is rejected before its payload is read.
        let error = unsafe { NumericMatrixRef::from_raw(&raw, "observations") }.unwrap_err();
        assert!(matches!(error, XllError::Input {
            argument: "observations",
            reason: InputError::TooLarge {
                limit: super::super::MAX_ARRAY_ELEMENTS,
                actual,
            },
        } if actual == usize::try_from(1_u64 << 32).unwrap_or(usize::MAX)));
    }

    #[test]
    fn miri_rejects_misaligned_numeric_headers_without_reading_them() {
        let raw = xlfn_sys::FP12 {
            rows: 1,
            columns: 1,
            array: [42.0],
        };
        let pointer = std::ptr::from_ref(&raw).cast::<u8>().wrapping_add(1).cast();
        // SAFETY: the deliberately misaligned pointer must be rejected before
        // the header or payload is accessed.
        let error = unsafe { NumericMatrixRef::from_raw(pointer, "observations") }.unwrap_err();
        assert!(matches!(
            error,
            XllError::Input {
                argument: "observations",
                reason: InputError::Malformed("misaligned numeric matrix pointer"),
            }
        ));
    }

    #[test]
    fn miri_numeric_validation_checks_every_block_and_tail_cell() {
        // Retain the SDK's concrete one-cell FP12 layout as well as the
        // flexible block/tail layouts below.
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let raw = xlfn_sys::FP12 {
                rows: 1,
                columns: 1,
                array: [number],
            };
            // SAFETY: the header and its single initialized double are live.
            let result = unsafe { NumericMatrixRef::from_raw(&raw, "matrix") };
            assert!(matches!(
                result,
                Err(XllError::Input {
                    argument: "matrix",
                    reason: InputError::NonFinite,
                })
            ));
        }
        let mut raw = NumericInput {
            rows: 1,
            columns: 129,
            values: [0.0; 129],
        };
        let finite = [-0.0, 0.0, f64::from_bits(1), f64::MAX, f64::MIN];
        for (index, value) in raw.values.iter_mut().enumerate() {
            *value = finite[index % finite.len()];
        }
        for length in [1, 63, 64, 65, 127, 128, 129] {
            raw.columns = length as i32;
            // SAFETY: the live header declares no more than the initialized payload.
            let matrix =
                unsafe { NumericMatrixRef::from_raw(std::ptr::from_ref(&raw).cast(), "matrix") }
                    .unwrap();
            assert_eq!(matrix.as_slice().as_ptr(), raw.values.as_ptr());
            assert_eq!(matrix.as_slice().len(), length);
            for (actual, expected) in matrix.iter().zip(raw.values) {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
            for index in 0..length {
                let saved = raw.values[index];
                for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                    raw.values[index] = invalid;
                    // SAFETY: the header and all declared doubles remain initialized.
                    let result = unsafe {
                        NumericMatrixRef::from_raw(std::ptr::from_ref(&raw).cast(), "matrix")
                    };
                    assert!(matches!(
                        result,
                        Err(XllError::Input {
                            argument: "matrix",
                            reason: InputError::NonFinite,
                        })
                    ));
                }
                raw.values[index] = saved;
            }
        }
    }

    fn assert_numeric_matrix_identity<const N: usize>() {
        use crate::value::input::{ArgumentContext, FormulaInputMode};
        let numeric: NumericInput<N> = NumericInput {
            rows: 1,
            columns: N as i32,
            values: std::array::from_fn(|index| match index % 6 {
                0 => -0.0,
                1 => 0.0,
                2 => -(index as f64) / 7.0,
                3 => f64::MIN_POSITIVE,
                4 => f64::from_bits(1),
                _ => f64::MAX,
            }),
        };
        let mut cells = numeric.values.map(xlfn_sys::XLOPER12::number);
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: 1,
                    columns: N as i32,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        };
        crate::call::with_excel_call_scope_and_state(&(raw, numeric), |(raw, numeric), scope| {
            // SAFETY: the flexible array owns N initialized doubles and is
            // immutable during this call-scoped identity comparison.
            let value =
                unsafe { NumericMatrixRef::from_raw(std::ptr::from_ref(numeric).cast(), "arg") }
                    .unwrap();
            let mut numeric_context = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            numeric_context
                .record_numeric_matrix(0, "arg", value)
                .unwrap();
            let mut strict_context = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            strict_context
                .prepare::<crate::value::Matrix<f64>>(
                    0,
                    "arg",
                    crate::value::XlValueRef::from_array_cell(raw).unwrap(),
                )
                .unwrap();
            assert_eq!(
                numeric_context.finish().unwrap(),
                strict_context.finish().unwrap()
            );
        });
    }

    #[test]
    fn miri_numeric_matrix_identity_matches_the_same_strict_numeric_values() {
        assert_numeric_matrix_identity::<2>();
        // Two dimension words precede the values: 14 values fill the inline
        // sink exactly. Exercise promotion and hash-buffer boundaries too.
        assert_numeric_matrix_identity::<14>();
        assert_numeric_matrix_identity::<15>();
        assert_numeric_matrix_identity::<510>();
        assert_numeric_matrix_identity::<511>();
    }
}
