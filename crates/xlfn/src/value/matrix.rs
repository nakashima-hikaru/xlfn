//! Owned rectangular and bounded collection values.
//!
//! With the `serde` feature, a matrix serializes as `rows`, `columns`, and its
//! row-major `data`. Rows, columns, and bounded varargs serialize as sequences.
//! Deserialization applies the same shape and length checks as construction.

use super::{EXCEL_MAX_COLUMNS, EXCEL_MAX_ROWS, MAX_ARRAY_ELEMENTS};
use crate::error::{DomainErrorCode, InputError, Shape};
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

    /// Iterates over mutable elements in row-major order.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.data.iter_mut()
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

fn validated_element_count(rows: usize, columns: usize) -> XllResult<usize> {
    let count = rows.checked_mul(columns).ok_or(XllError::Domain {
        code: DomainErrorCode::Overflow,
    })?;
    validate_matrix_dimensions(rows, columns, count)?;
    Ok(count)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

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
