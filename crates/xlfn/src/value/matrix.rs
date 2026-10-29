//! Owned rectangular and bounded collection values.
//!
//! With the `serde` feature, a matrix serializes as `rows`, `columns`, and its
//! row-major `data`. Rows, columns, and bounded varargs serialize as sequences.
//! Deserialization applies the same shape and length checks as construction.

use super::{EXCEL_MAX_COLUMNS, EXCEL_MAX_ROWS, MAX_ARRAY_ELEMENTS};
use crate::error::{InputError, Shape};
use crate::{XllError, XllResult};
use std::ops::{Deref, DerefMut, Index, IndexMut};

/// An owned, non-empty rectangular collection in row-major order.
///
/// Construction checks dimensions, cell count, and Excel/framework limits.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
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

    /// Creates a matrix by calling `make(row, column)` in row-major order.
    ///
    /// Invalid dimensions are rejected before allocating or calling `make`.
    pub fn from_fn(
        rows: usize,
        columns: usize,
        mut make: impl FnMut(usize, usize) -> T,
    ) -> XllResult<Self> {
        let count = validated_element_count(rows, columns)?;
        let mut data = Vec::with_capacity(count);
        for row in 0..rows {
            for column in 0..columns {
                data.push(make(row, column));
            }
        }
        Ok(Self {
            rows,
            columns,
            data,
        })
    }

    /// Creates a matrix filled with clones of `value`.
    ///
    /// Invalid dimensions are rejected before allocating or cloning `value`.
    pub fn fill(rows: usize, columns: usize, value: T) -> XllResult<Self>
    where
        T: Clone,
    {
        let count = validated_element_count(rows, columns)?;
        Ok(Self {
            rows,
            columns,
            data: vec![value; count],
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

    /// Returns all mutable elements in row-major order without changing shape.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.data
    }

    /// Returns a zero-based element, or `None` if either axis is out of bounds
    /// or the index calculation overflows.
    #[must_use]
    pub fn get(&self, row: usize, column: usize) -> Option<&T> {
        let index = checked_index(self.rows, self.columns, row, column)?;
        self.data.get(index)
    }

    /// Returns a mutable zero-based element, or `None` if either axis is out of
    /// bounds or the index calculation overflows.
    #[must_use]
    pub fn get_mut(&mut self, row: usize, column: usize) -> Option<&mut T> {
        let index = checked_index(self.rows, self.columns, row, column)?;
        self.data.get_mut(index)
    }

    /// Consumes the matrix and returns its row-major elements.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.data
    }

    /// Returns a zero-based row, or `None` when it is out of bounds.
    pub fn row(&self, row: usize) -> Option<&[T]> {
        if row >= self.rows {
            return None;
        }
        let start = row.checked_mul(self.columns)?;
        let end = start.checked_add(self.columns)?;
        self.data.get(start..end)
    }

    /// Returns a mutable zero-based row, or `None` when it is out of bounds.
    pub fn row_mut(&mut self, row: usize) -> Option<&mut [T]> {
        if row >= self.rows {
            return None;
        }
        let start = row.checked_mul(self.columns)?;
        let end = start.checked_add(self.columns)?;
        self.data.get_mut(start..end)
    }

    /// Iterates over disjoint mutable rows, from top to bottom.
    pub fn rows_mut(&mut self) -> impl ExactSizeIterator<Item = &mut [T]> + '_ {
        self.data.chunks_exact_mut(self.columns)
    }

    /// Iterates over a zero-based column, or returns `None` out of bounds.
    pub fn column(&self, column: usize) -> Option<impl Iterator<Item = &T>> {
        (column < self.columns).then(|| self.data.iter().skip(column).step_by(self.columns))
    }

    /// Iterates over all elements in row-major order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.data.iter()
    }

    /// Iterates over mutable elements in row-major order.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.data.iter_mut()
    }

    /// Consumes the matrix, mapping elements in row-major order while keeping
    /// its shape. Stops at the first error and drops any mapped output and
    /// unprocessed input elements.
    pub fn try_map<U, E>(self, f: impl FnMut(T) -> Result<U, E>) -> Result<Matrix<U>, E> {
        let Self {
            rows,
            columns,
            data,
        } = self;
        let data = data.into_iter().map(f).collect::<Result<Vec<_>, _>>()?;
        Ok(Matrix {
            rows,
            columns,
            data,
        })
    }
}

impl<T> TryFrom<Vec<Vec<T>>> for Matrix<T> {
    type Error = XllError;

    fn try_from(rows: Vec<Vec<T>>) -> XllResult<Self> {
        let row_count = rows.len();
        let column_count = rows.first().map_or(0, Vec::len);
        let count = validated_element_count(row_count, column_count)?;
        for row in &rows {
            if row.len() != column_count {
                return Err(XllError::input(
                    "<matrix>",
                    InputError::Malformed("matrix rows must have equal lengths"),
                ));
            }
        }
        let mut data = Vec::with_capacity(count);
        for row in rows {
            data.extend(row);
        }
        Ok(Self {
            rows: row_count,
            columns: column_count,
            data,
        })
    }
}

impl<T> AsRef<[T]> for Matrix<T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> IntoIterator for Matrix<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Matrix<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Matrix<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

#[cfg(feature = "serde")]
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Matrix<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct SerializedMatrix<T> {
            rows: usize,
            columns: usize,
            data: Vec<T>,
        }

        let value = SerializedMatrix::deserialize(deserializer)?;
        Self::new(value.rows, value.columns, value.data).map_err(serde::de::Error::custom)
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

    /// Returns a zero-based element borrowed for the input call lifetime, or
    /// `None` if either axis is out of bounds or the index calculation overflows.
    #[must_use]
    pub fn get(&self, row: usize, column: usize) -> Option<&'call T> {
        let index = checked_index(self.rows, self.columns, row, column)?;
        self.data.get(index)
    }

    /// Returns a zero-based borrowed row, or `None` out of bounds.
    pub fn row(&self, row: usize) -> Option<&'call [T]> {
        if row >= self.rows {
            return None;
        }
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

impl<T> IndexMut<(usize, usize)> for Matrix<T> {
    fn index_mut(&mut self, (row, column): (usize, usize)) -> &mut Self::Output {
        assert!(row < self.rows, "matrix row index out of bounds");
        assert!(column < self.columns, "matrix column index out of bounds");
        &mut self.data[row * self.columns + column]
    }
}

impl<T> Index<(usize, usize)> for MatrixRef<'_, T> {
    type Output = T;

    fn index(&self, (row, column): (usize, usize)) -> &Self::Output {
        assert!(row < self.rows, "matrix row index out of bounds");
        assert!(column < self.columns, "matrix column index out of bounds");
        &self.data[row * self.columns + column]
    }
}

impl<T> AsRef<[T]> for MatrixRef<'_, T> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<'call, T> IntoIterator for MatrixRef<'call, T> {
    type Item = &'call T;
    type IntoIter = std::slice::Iter<'call, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

impl<'call, T> IntoIterator for &MatrixRef<'call, T> {
    type Item = &'call T;
    type IntoIter = std::slice::Iter<'call, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

fn checked_index(rows: usize, columns: usize, row: usize, column: usize) -> Option<usize> {
    if row >= rows || column >= columns {
        return None;
    }
    row.checked_mul(columns)?.checked_add(column)
}

fn validated_element_count(rows: usize, columns: usize) -> XllResult<usize> {
    matrix_element_count(rows, columns, "<matrix>")
}

pub(crate) fn validate_matrix_dimensions(
    rows: usize,
    columns: usize,
    actual: usize,
) -> XllResult<()> {
    validate_matrix_dimensions_for_argument(rows, columns, actual, "<matrix>")
}

pub(super) fn validate_matrix_dimensions_for_argument(
    rows: usize,
    columns: usize,
    actual: usize,
    argument: &'static str,
) -> XllResult<()> {
    let expected = matrix_element_count(rows, columns, argument)?;
    if expected != actual {
        return Err(XllError::ElementCountMismatch {
            rows,
            columns,
            expected,
            actual,
        });
    }
    Ok(())
}

pub(super) fn matrix_element_count(
    rows: usize,
    columns: usize,
    argument: &'static str,
) -> XllResult<usize> {
    if rows == 0 || columns == 0 {
        return Err(XllError::input(
            argument,
            InputError::Malformed("matrix dimensions must be non-zero"),
        ));
    }
    array_element_count(rows, columns, argument)
}

/// Validates worksheet axes and the framework cell limit before narrowing
/// the element count. Empty arrays remain available to vectors and raw views.
pub(super) fn array_element_count(
    rows: usize,
    columns: usize,
    argument: &'static str,
) -> XllResult<usize> {
    if rows > EXCEL_MAX_ROWS {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit: EXCEL_MAX_ROWS,
                actual: rows,
            },
        ));
    }
    if columns > EXCEL_MAX_COLUMNS {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit: EXCEL_MAX_COLUMNS,
                actual: columns,
            },
        ));
    }
    // Both axes fit Excel's limits, so their product fits u64 even on x86.
    let elements = rows as u64 * columns as u64;
    if elements > MAX_ARRAY_ELEMENTS as u64 {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit: MAX_ARRAY_ELEMENTS,
                // InputError reports platform-sized counts; values too large
                // to represent saturate while retaining the input error kind.
                actual: usize::try_from(elements).unwrap_or(usize::MAX),
            },
        ));
    }
    Ok(elements as usize)
}

/// An owned, non-empty row with explicit worksheet orientation.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
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

macro_rules! impl_sequence_traits {
    ($name:ident $(, $max:ident)?) => {
        impl<T $(, const $max: usize)?> Deref for $name<T $(, $max)?> {
            type Target = [T];

            fn deref(&self) -> &Self::Target {
                self.as_slice()
            }
        }

        impl<T $(, const $max: usize)?> DerefMut for $name<T $(, $max)?> {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        impl<T $(, const $max: usize)?> AsRef<[T]> for $name<T $(, $max)?> {
            fn as_ref(&self) -> &[T] {
                self.as_slice()
            }
        }

        impl<T, I $(, const $max: usize)?> Index<I> for $name<T $(, $max)?>
        where
            I: std::slice::SliceIndex<[T]>,
        {
            type Output = I::Output;

            fn index(&self, index: I) -> &Self::Output {
                &self.0[index]
            }
        }

        impl<T, I $(, const $max: usize)?> IndexMut<I> for $name<T $(, $max)?>
        where
            I: std::slice::SliceIndex<[T]>,
        {
            fn index_mut(&mut self, index: I) -> &mut Self::Output {
                &mut self.0[index]
            }
        }

        impl<T $(, const $max: usize)?> IntoIterator for $name<T $(, $max)?> {
            type Item = T;
            type IntoIter = std::vec::IntoIter<T>;

            fn into_iter(self) -> Self::IntoIter {
                self.0.into_iter()
            }
        }

        impl<'a, T $(, const $max: usize)?> IntoIterator for &'a $name<T $(, $max)?> {
            type Item = &'a T;
            type IntoIter = std::slice::Iter<'a, T>;

            fn into_iter(self) -> Self::IntoIter {
                self.0.iter()
            }
        }

        impl<'a, T $(, const $max: usize)?> IntoIterator for &'a mut $name<T $(, $max)?> {
            type Item = &'a mut T;
            type IntoIter = std::slice::IterMut<'a, T>;

            fn into_iter(self) -> Self::IntoIter {
                self.0.iter_mut()
            }
        }

        #[cfg(feature = "serde")]
        impl<'de, T: serde::Deserialize<'de> $(, const $max: usize)?> serde::Deserialize<'de>
            for $name<T $(, $max)?>
        {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let values = Vec::<T>::deserialize(deserializer)?;
                Self::new(values).map_err(serde::de::Error::custom)
            }
        }
    };
}

impl_sequence_traits!(Row);
impl_sequence_traits!(Column);
impl_sequence_traits!(BoundedVarArgs, MAX);

macro_rules! impl_oriented_conversions {
    ($name:ident) => {
        impl<T> AsMut<[T]> for $name<T> {
            fn as_mut(&mut self) -> &mut [T] {
                &mut self.0
            }
        }

        impl<T> TryFrom<Vec<T>> for $name<T> {
            type Error = XllError;

            fn try_from(values: Vec<T>) -> XllResult<Self> {
                Self::new(values)
            }
        }

        impl<T> From<$name<T>> for Vec<T> {
            fn from(value: $name<T>) -> Self {
                value.into_vec()
            }
        }
    };
}

impl_oriented_conversions!(Row);
impl_oriented_conversions!(Column);

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn checked_accessors_validate_axes_before_flattening() {
        let mut matrix = Matrix::new(2, 3, vec![0, 1, 2, 3, 4, 5]).unwrap();
        for row in 0..2 {
            for column in 0..3 {
                assert_eq!(matrix.get(row, column), Some(&(row * 3 + column)));
                assert_eq!(matrix.get_mut(row, column), Some(&mut (row * 3 + column)));
            }
        }
        for (row, column) in [
            (0, 3),
            (0, 4),
            (1, 3),
            (2, 0),
            (usize::MAX, 0),
            (0, usize::MAX),
            (usize::MAX, usize::MAX),
        ] {
            assert_eq!(matrix.get(row, column), None);
            assert_eq!(matrix.get_mut(row, column), None);
            let view = MatrixRef::from_slice(2, 3, matrix.as_slice()).unwrap();
            assert_eq!(view.get(row, column), None);
        }
        // Valid matrices cannot overflow their index calculation. Exercise the
        // checked arithmetic independently to retain that defense explicitly.
        assert_eq!(checked_index(usize::MAX, 2, usize::MAX - 1, 0), None);
        assert_eq!(checked_index(3, usize::MAX, 1, usize::MAX - 1), None);
        assert_eq!(matrix.as_slice(), &[0, 1, 2, 3, 4, 5]);
        let view = MatrixRef::from_slice(2, 3, matrix.as_slice()).unwrap();
        assert_eq!(view.get(0, 0), Some(&0));
        assert_eq!(view.get(1, 2), Some(&5));
    }

    #[test]
    fn borrowed_accessor_reference_uses_the_input_lifetime() {
        let values = [1, 2, 3, 4];
        let element = {
            let view = MatrixRef::from_slice(2, 2, &values).unwrap();
            view.get(1, 1).unwrap()
        };
        assert_eq!(*element, 4);
    }

    #[test]
    fn mutable_slices_and_rows_preserve_shape_and_are_disjoint() {
        let mut matrix = Matrix::new(2, 3, vec![0, 1, 2, 3, 4, 5]).unwrap();
        let shape = matrix.shape();
        matrix.as_mut_slice().reverse();
        *matrix.get_mut(0, 0).unwrap() = 10;
        matrix.row_mut(1).unwrap().copy_from_slice(&[20, 30, 40]);
        assert_eq!(matrix.row_mut(2), None);
        assert_eq!(matrix.row_mut(usize::MAX), None);
        {
            let mut rows = matrix.rows_mut();
            assert_eq!(rows.len(), 2);
            let first = rows.next().unwrap();
            assert_eq!(rows.len(), 1);
            let second = rows.next().unwrap();
            first[1] = 50;
            second[2] = 60;
            assert_eq!(rows.len(), 0);
            assert!(rows.next().is_none());
        }
        assert_eq!(matrix.shape(), shape);
        assert_eq!(matrix.as_slice(), &[10, 50, 3, 20, 30, 60]);
    }

    #[test]
    fn try_map_preserves_shape_and_row_major_order() {
        let matrix = Matrix::new(2, 3, vec![0, 1, 2, 3, 4, 5]).unwrap();
        let mut calls = Vec::new();
        let mapped = matrix
            .try_map::<_, ()>(|value| {
                calls.push(value);
                Ok(value.to_string())
            })
            .unwrap();
        assert_eq!(calls, [0, 1, 2, 3, 4, 5]);
        assert_eq!(
            mapped.shape(),
            Shape {
                rows: 2,
                columns: 3
            }
        );
        assert_eq!(mapped.as_slice(), &["0", "1", "2", "3", "4", "5"]);
    }

    #[test]
    fn try_map_returns_first_error_and_drops_inputs_and_partial_outputs() {
        struct Tracked<'a> {
            index: usize,
            drops: &'a [Cell<usize>],
        }
        impl Drop for Tracked<'_> {
            fn drop(&mut self) {
                let count = &self.drops[self.index];
                count.set(count.get() + 1);
            }
        }
        let input_drops = [const { Cell::new(0) }; 6];
        let output_drops = [const { Cell::new(0) }; 6];
        let matrix = Matrix::from_fn(2, 3, |row, column| Tracked {
            index: row * 3 + column,
            drops: &input_drops,
        })
        .unwrap();
        let mut calls = Vec::new();
        let result = matrix.try_map(|input| {
            calls.push(input.index);
            if input.index >= 2 {
                Err(input.index)
            } else {
                Ok(Tracked {
                    index: input.index,
                    drops: &output_drops,
                })
            }
        });
        assert!(matches!(result, Err(2)));
        assert_eq!(calls, [0, 1, 2]);
        assert_eq!(input_drops.each_ref().map(Cell::get), [1; 6]);
        assert_eq!(output_drops.each_ref().map(Cell::get), [1, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn oriented_standard_conversions_keep_length_limits_and_storage() {
        let mut row = Row::try_from(vec![1, 2, 3]).unwrap();
        let pointer = row.as_slice().as_ptr();
        AsMut::<[i32]>::as_mut(&mut row).reverse();
        let row_values = Vec::from(row);
        assert_eq!(row_values, [3, 2, 1]);
        assert_eq!(row_values.as_ptr(), pointer);

        let mut column = Column::try_from(vec![1, 2, 3]).unwrap();
        let pointer = column.as_slice().as_ptr();
        AsMut::<[i32]>::as_mut(&mut column)[1] = 10;
        let column_values = Vec::from(column);
        assert_eq!(column_values, [1, 10, 3]);
        assert_eq!(column_values.as_ptr(), pointer);

        assert!(Row::<()>::try_from(vec![]).is_err());
        assert!(Column::<()>::try_from(vec![]).is_err());
        assert!(Row::try_from(vec![(); EXCEL_MAX_COLUMNS]).is_ok());
        assert!(Row::try_from(vec![(); EXCEL_MAX_COLUMNS + 1]).is_err());
        assert!(Column::try_from(vec![(); EXCEL_MAX_ROWS.min(MAX_ARRAY_ELEMENTS)]).is_ok());
        assert!(Column::try_from(vec![(); EXCEL_MAX_ROWS + 1]).is_err());
        assert!(Column::try_from(vec![(); MAX_ARRAY_ELEMENTS + 1]).is_err());
    }

    #[test]
    fn factories_preserve_row_major_order_and_rectangular_shape() {
        let matrix = Matrix::from_fn(2, 3, |row, column| row * 10 + column).unwrap();
        assert_eq!(
            matrix.shape(),
            Shape {
                rows: 2,
                columns: 3
            }
        );
        assert_eq!(matrix.as_slice(), &[0, 1, 2, 10, 11, 12]);
        assert_eq!(
            Matrix::fill(2, 2, String::from("cell")).unwrap().as_slice(),
            &["cell"; 4]
        );
        assert_eq!(
            Matrix::try_from(vec![vec![0, 1, 2], vec![10, 11, 12]]).unwrap(),
            matrix
        );
        assert!(Matrix::<i32>::try_from(vec![]).is_err());
        assert!(Matrix::<i32>::try_from(vec![vec![]]).is_err());
        assert!(Matrix::try_from(vec![vec![1, 2], vec![3]]).is_err());
        assert!(Matrix::try_from(vec![vec![1], vec![]]).is_err());
    }

    #[test]
    fn invalid_factories_reject_dimensions_before_user_callbacks_or_clones() {
        let calls = Cell::new(0);
        for (rows, columns) in [
            (0, 1),
            (1, 0),
            (EXCEL_MAX_ROWS + 1, 1),
            (1, EXCEL_MAX_COLUMNS + 1),
            (EXCEL_MAX_ROWS, EXCEL_MAX_COLUMNS),
            (usize::MAX, 2),
        ] {
            let result = Matrix::from_fn(rows, columns, |_, _| {
                calls.set(calls.get() + 1);
            });
            assert!(result.is_err());
        }
        assert_eq!(calls.get(), 0);

        struct CloneTracked<'a>(&'a Cell<usize>);
        impl Clone for CloneTracked<'_> {
            fn clone(&self) -> Self {
                self.0.set(self.0.get() + 1);
                Self(self.0)
            }
        }
        let clones = Cell::new(0);
        assert!(Matrix::fill(0, 1, CloneTracked(&clones)).is_err());
        assert!(Matrix::fill(1, EXCEL_MAX_COLUMNS + 1, CloneTracked(&clones)).is_err());
        assert_eq!(clones.get(), 0);
    }

    #[test]
    fn matrix_mutation_and_iteration_preserve_shape_and_order() {
        let mut matrix = Matrix::new(
            2,
            2,
            vec![
                String::from("a"),
                String::from("b"),
                String::from("c"),
                String::from("d"),
            ],
        )
        .unwrap();
        matrix[(1, 0)] = String::from("changed");
        for value in &mut matrix {
            value.push('!');
        }
        assert_eq!(
            (&matrix)
                .into_iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a!", "b!", "changed!", "d!"]
        );
        assert_eq!(
            matrix.shape(),
            Shape {
                rows: 2,
                columns: 2
            }
        );
        assert_eq!(
            matrix.into_iter().collect::<Vec<_>>(),
            ["a!", "b!", "changed!", "d!"]
        );
    }

    #[test]
    fn mutable_and_borrowed_indices_reject_each_invalid_axis() {
        let mut matrix = Matrix::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        for index in [(0, 2), (2, 0), (usize::MAX, 1), (1, usize::MAX)] {
            assert!(
                crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                    matrix[index] = 99;
                }))
                .is_err()
            );
        }
        assert_eq!(matrix.as_slice(), &[1, 2, 3, 4]);
        let borrowed = MatrixRef::from_slice(2, 2, matrix.as_slice()).unwrap();
        assert_eq!(borrowed[(1, 1)], 4);
        for index in [(0, 2), (2, 0), (usize::MAX, 1), (1, usize::MAX)] {
            assert!(crate::panic_boundary::catch_no_unwind(|| borrowed[index]).is_err());
        }
        assert_eq!(
            (&borrowed).into_iter().copied().collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            borrowed.into_iter().copied().collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
    }

    #[test]
    fn oriented_and_bounded_sequences_support_slice_operations_without_resizing() {
        macro_rules! check {
            ($value:expr) => {
                let mut value = $value;
                assert_eq!(value.len(), 3);
                assert_eq!(&value[1..], &[2, 3]);
                value[0] = 10;
                value.reverse();
                for element in &mut value {
                    *element += 1;
                }
                assert_eq!(AsRef::<[i32]>::as_ref(&value), &[4, 3, 11]);
                assert_eq!(
                    (&value).into_iter().copied().collect::<Vec<_>>(),
                    [4, 3, 11]
                );
                assert_eq!(value.into_iter().collect::<Vec<_>>(), [4, 3, 11]);
            };
        }
        check!(Row::new(vec![1, 2, 3]).unwrap());
        check!(Column::new(vec![1, 2, 3]).unwrap());
        check!(BoundedVarArgs::<_, 3>::new(vec![1, 2, 3]).unwrap());
        assert_eq!(
            BoundedVarArgs::<i32, 3>::new(vec![])
                .unwrap()
                .into_iter()
                .len(),
            0
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_roundtrips_owned_shapes_and_rejects_invalid_dimensions() {
        let matrix = Matrix::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        let json = serde_json::to_string(&matrix).unwrap();
        assert_eq!(json, r#"{"rows":2,"columns":2,"data":[1,2,3,4]}"#);
        assert_eq!(serde_json::from_str::<Matrix<i32>>(&json).unwrap(), matrix);
        for json in [
            r#"{"rows":0,"columns":1,"data":[]}"#,
            r#"{"rows":1,"columns":0,"data":[]}"#,
            r#"{"rows":2,"columns":2,"data":[1,2,3]}"#,
            r#"{"rows":1048577,"columns":1,"data":[]}"#,
            r#"{"rows":1,"columns":16385,"data":[]}"#,
        ] {
            assert!(serde_json::from_str::<Matrix<i32>>(json).is_err());
        }
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_sequence_deserialization_preserves_orientation_and_length_limits() {
        let row = Row::new(vec![1, 2, 3]).unwrap();
        let column = Column::new(vec![1, 2, 3]).unwrap();
        let bounded = BoundedVarArgs::<_, 3>::new(vec![1, 2, 3]).unwrap();
        assert_eq!(serde_json::to_string(&row).unwrap(), "[1,2,3]");
        assert_eq!(serde_json::from_str::<Row<i32>>("[1,2,3]").unwrap(), row);
        assert_eq!(
            serde_json::from_str::<Column<i32>>("[1,2,3]").unwrap(),
            column
        );
        assert_eq!(
            serde_json::from_str::<BoundedVarArgs<i32, 3>>("[1,2,3]").unwrap(),
            bounded
        );
        assert!(serde_json::from_str::<Row<i32>>("[]").is_err());
        assert!(serde_json::from_str::<Column<i32>>("[]").is_err());
        assert!(serde_json::from_str::<BoundedVarArgs<i32, 0>>("[]").is_err());
        assert!(serde_json::from_str::<BoundedVarArgs<i32, 2>>("[1,2,3]").is_err());
        assert!(serde_json::from_str::<BoundedVarArgs<i32, 3>>("[]").is_ok());
    }
}
