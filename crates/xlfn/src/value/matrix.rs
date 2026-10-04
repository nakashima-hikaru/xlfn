//! Owned rectangular and bounded collection values.

use super::{EXCEL_MAX_COLUMNS, EXCEL_MAX_ROWS, MAX_ARRAY_ELEMENTS};
use crate::error::{DomainErrorCode, InputError, Shape};
use crate::{XllError, XllResult};
use std::ops::Index;

/// An owned, non-empty rectangular collection in row-major order.
///
/// Construction checks dimensions, cell count, and Excel/framework limits.
#[derive(Clone, Debug, PartialEq)]
pub struct Matrix<T> {
    pub(super) rows: usize,
    pub(super) columns: usize,
    pub(super) data: Vec<T>,
}

impl<T> Matrix<T> {
    /// Validates `rows × columns == data.len()` and the dimension limits.
    pub fn new(rows: usize, columns: usize, data: Vec<T>) -> XllResult<Self> {
        validate_matrix_dimensions(rows, columns, data.len())?;
        Ok(Self {
            rows,
            columns,
            data,
        })
    }

    /// Returns the number of rows.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Returns the number of columns.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    /// Returns the rectangular shape used by conversion diagnostics.
    #[must_use]
    pub const fn shape(&self) -> Shape {
        Shape {
            rows: self.rows,
            columns: self.columns,
        }
    }

    /// Returns all elements in row-major order.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.data
    }

    /// Consumes the matrix and returns its row-major elements.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.data
    }

    /// Returns a zero-based row, or `None` when it is out of bounds.
    pub fn row(&self, row: usize) -> Option<&[T]> {
        let start = row.checked_mul(self.columns)?;
        let end = start.checked_add(self.columns)?;
        self.data.get(start..end)
    }

    /// Iterates over a zero-based column, or returns `None` out of bounds.
    pub fn column(&self, column: usize) -> Option<impl Iterator<Item = &T>> {
        (column < self.columns).then(|| self.data.iter().skip(column).step_by(self.columns))
    }

    /// Iterates over all elements in row-major order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.data.iter()
    }
}

/// A call-scoped view over a typed rectangular collection materialized in the
/// active [`crate::call::CallScope`] scratch arena.
///
/// `MatrixRef` does not own a separate heap allocation and cannot outlive the
/// Excel call that created it. Excel stores an input array as `XLOPER12`
/// cells, so decoding into a typed `&[T]` necessarily copies the elements;
/// this type avoids per-element ownership and deallocation rather than being
/// a literal zero-copy view. Use [`crate::value::XlArrayRef`] for lazy access
/// to the raw cells, or [`Self::to_owned`] when the values must cross the
/// call boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatrixRef<'call, T> {
    rows: usize,
    columns: usize,
    data: &'call [T],
}

impl<'call, T> MatrixRef<'call, T> {
    pub(crate) fn from_slice(rows: usize, columns: usize, data: &'call [T]) -> XllResult<Self> {
        validate_matrix_dimensions(rows, columns, data.len())?;
        Ok(Self {
            rows,
            columns,
            data,
        })
    }

    /// Returns the number of rows.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Returns the number of columns.
    #[must_use]
    pub const fn columns(&self) -> usize {
        self.columns
    }

    /// Returns the rectangular shape used by conversion diagnostics.
    #[must_use]
    pub const fn shape(&self) -> Shape {
        Shape {
            rows: self.rows,
            columns: self.columns,
        }
    }

    /// Returns elements borrowed for the input call lifetime.
    #[must_use]
    pub const fn as_slice(&self) -> &'call [T] {
        self.data
    }

    /// Returns a zero-based borrowed row, or `None` out of bounds.
    pub fn row(&self, row: usize) -> Option<&'call [T]> {
        let start = row.checked_mul(self.columns)?;
        let end = start.checked_add(self.columns)?;
        self.data.get(start..end)
    }

    /// Iterates over a zero-based borrowed column, or returns `None` out of bounds.
    pub fn column(&self, column: usize) -> Option<impl Iterator<Item = &'call T>> {
        (column < self.columns).then(|| self.data.iter().skip(column).step_by(self.columns))
    }

    /// Iterates over borrowed elements in row-major order.
    pub fn iter(&self) -> std::slice::Iter<'call, T> {
        self.data.iter()
    }

    /// Clones the elements into an owned matrix that can outlive the call.
    pub fn to_owned(&self) -> XllResult<Matrix<T>>
    where
        T: Clone,
    {
        Matrix::new(self.rows, self.columns, self.data.to_vec())
    }
}

impl<T> Index<(usize, usize)> for Matrix<T> {
    type Output = T;

    fn index(&self, (row, column): (usize, usize)) -> &Self::Output {
        assert!(row < self.rows, "matrix row index out of bounds");
        assert!(column < self.columns, "matrix column index out of bounds");
        let index = row
            .checked_mul(self.columns)
            .and_then(|index| index.checked_add(column))
            .expect("matrix index overflow");
        &self.data[index]
    }
}

pub(crate) fn validate_matrix_dimensions(
    rows: usize,
    columns: usize,
    actual: usize,
) -> XllResult<()> {
    if rows == 0 || columns == 0 {
        return Err(XllError::input(
            "<matrix>",
            InputError::Malformed("matrix dimensions must be non-zero"),
        ));
    }
    if rows > EXCEL_MAX_ROWS {
        return Err(XllError::input(
            "<matrix>",
            InputError::TooLarge {
                limit: EXCEL_MAX_ROWS,
                actual: rows,
            },
        ));
    }
    if columns > EXCEL_MAX_COLUMNS {
        return Err(XllError::input(
            "<matrix>",
            InputError::TooLarge {
                limit: EXCEL_MAX_COLUMNS,
                actual: columns,
            },
        ));
    }
    let expected = rows.checked_mul(columns).ok_or(XllError::Domain {
        code: DomainErrorCode::Overflow,
    })?;
    if expected != actual {
        return Err(XllError::ElementCountMismatch {
            rows,
            columns,
            expected,
            actual,
        });
    }
    if expected > MAX_ARRAY_ELEMENTS {
        return Err(XllError::input(
            "<matrix>",
            InputError::TooLarge {
                limit: MAX_ARRAY_ELEMENTS,
                actual: expected,
            },
        ));
    }
    Ok(())
}

/// An owned, non-empty row with explicit worksheet orientation.
#[derive(Clone, Debug, PartialEq)]
pub struct Row<T>(pub(super) Vec<T>);

impl<T> Row<T> {
    /// Creates one row, validating non-empty length and Excel limits.
    pub fn new(data: Vec<T>) -> XllResult<Self> {
        let matrix = Matrix::new(1, data.len(), data)?;
        Ok(Self(matrix.into_vec()))
    }
    /// Returns the elements from left to right.
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
    /// Consumes the row and returns its elements.
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

/// An owned, non-empty column with explicit worksheet orientation.
#[derive(Clone, Debug, PartialEq)]
pub struct Column<T>(pub(super) Vec<T>);

impl<T> Column<T> {
    /// Creates one column, validating non-empty length and Excel limits.
    pub fn new(data: Vec<T>) -> XllResult<Self> {
        let matrix = Matrix::new(data.len(), 1, data)?;
        Ok(Self(matrix.into_vec()))
    }
    /// Returns the elements from top to bottom.
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
    /// Consumes the column and returns its elements.
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

/// Input-only one-dimensional arguments bounded by `MAX` elements.
///
/// Unlike a row or column, the container does not preserve orientation and may
/// be empty. `MAX` must be non-zero.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundedVarArgs<T, const MAX: usize>(pub(super) Vec<T>);

impl<T, const MAX: usize> BoundedVarArgs<T, MAX> {
    /// Rejects a zero maximum or a length greater than `MAX`.
    pub fn new(values: Vec<T>) -> XllResult<Self> {
        if MAX == 0 {
            return Err(XllError::input(
                "<varargs>",
                InputError::Malformed("bounded varargs maximum must be non-zero"),
            ));
        }
        if values.len() > MAX {
            return Err(XllError::input(
                "<varargs>",
                InputError::TooLarge {
                    limit: MAX,
                    actual: values.len(),
                },
            ));
        }
        Ok(Self(values))
    }

    /// Returns the decoded elements in their input order.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// Consumes the bounded input and returns its elements.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}
