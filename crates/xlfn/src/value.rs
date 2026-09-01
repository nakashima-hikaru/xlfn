use crate::error::{DomainErrorCode, InputError, Shape};
use crate::{ExcelError, XllError, XllResult};
use xlfn_sys::XLOPER12;
#[cfg(test)]
use xlfn_sys::XLOPER12Array;

/// Borrowed call-scoped views used while converting one worksheet call.
pub mod borrowed;
/// Excel serial-date policy and value types.
pub mod date;
/// Internal semantic identity support used by generated input conversion.
#[doc(hidden)]
pub mod identity;
/// Input conversion traits and presence/default handling.
#[allow(
    unsafe_code,
    reason = "Raw XLOPER12 input conversion is isolated in this leaf"
)]
pub(crate) mod input;
/// Owned rectangular and bounded collection values.
pub mod matrix;
/// Output conversion traits and return-cell representations.
pub(crate) mod output;
/// Raw, borrowed views over Excel's XLOPER12 input representation.
#[allow(unsafe_code, reason = "Raw XLOPER12 views are the value ABI leaf")]
pub mod raw;

#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use crate::call::with_excel_call_scope;
pub use crate::input_identity::InputIdentityEncoder;
#[cfg(any(test, feature = "handles", feature = "bench-internals"))]
pub(crate) use input::FormulaInputMode;
#[cfg(test)]
pub(crate) use input::argument_from_raw;
#[cfg(all(test, feature = "handles"))]
pub(crate) use input::argument_from_raw_with_context;
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use input::{ArgumentContext, argument_from_raw_with_arguments};
pub(crate) use input::{CallContext, ExcelParameter};
pub use input::{ExcelInputIdentity, FromExcel};
pub(crate) use input::{InputMode, PlainInputMode};
pub use output::IntoExcel;

pub use date::{ExcelDateSystem, ExcelSerialDate};
pub(crate) use matrix::validate_matrix_dimensions;
pub use matrix::{BoundedVarArgs, Column, Matrix, MatrixRef, Row};
pub(crate) use raw::{GridView, encode_raw_value};
pub use raw::{XlArrayRef, XlStrRef, XlValueRef, XlValueType};

const EXCEL_MAX_ROWS: usize = 1_048_576;
const EXCEL_MAX_COLUMNS: usize = 16_384;
const MAX_ARRAY_ELEMENTS: usize = core::cfg_select! {
    target_pointer_width = "32" => 1_000_000,
    _ => 4_000_000,
};
pub(crate) const MAX_ARRAY_BYTES: usize = core::cfg_select! {
    target_pointer_width = "32" => 64 * 1024 * 1024,
    _ => 256 * 1024 * 1024,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExcelErrorValue(pub ExcelError);

#[derive(Clone, Debug, PartialEq)]
pub enum ExcelCellValue {
    Number(f64),
    Boolean(bool),
    String(String),
    Error(ExcelError),
    Blank,
}

/// A zero-allocation view of one worksheet cell for a synchronous call.
///
/// The string variant borrows UTF-8 text from the active call scope. This
/// type is therefore suitable for synchronous and main-thread UDFs, but it
/// cannot be moved into an asynchronous future.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExcelCellRef<'call> {
    Number(f64),
    Boolean(bool),
    String(&'call str),
    Error(ExcelError),
    Blank,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExcelValue {
    Scalar(ExcelCellValue),
    Missing,
    Array(Matrix<ExcelCellValue>),
}

/// A single worksheet cell in the final semantic return representation.
///
/// Unlike [`ExcelCellValue`], this type cannot represent an omitted or blank
/// cell. Use an explicit empty string or [`ExcelError::NotAvailable`] when that
/// is the intended worksheet result.
#[derive(Clone, Debug, PartialEq)]
pub enum ExcelCellOutput {
    Number(f64),
    Boolean(bool),
    String(String),
    Error(ExcelError),
}

/// An input-only distinction between an omitted and a blank Excel argument.
///
/// Excel does not preserve these meanings for UDF return values: both are
/// displayed as numeric zero. Return an explicit value, empty string, or
/// `ExcelErrorValue` instead.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionalExcelValue<T> {
    Missing,
    Blank,
    Value(T),
}

#[allow(
    unsafe_code,
    reason = "XLOPER12 numeric union projection is audited here"
)]
impl<'call> FromExcel<'call> for f64 {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let number = match value.value_type() {
            // SAFETY: The root type selects the corresponding union member.
            XlValueType::Number => unsafe { value.raw.value.number },
            // SAFETY: The root type selects the corresponding union member.
            XlValueType::Integer => (unsafe { value.raw.value.integer }) as f64,
            _ => return Err(value.wrong_type(argument, "number")),
        };
        if !number.is_finite() {
            return Err(XllError::input(argument, InputError::NonFinite));
        }
        Ok(number)
    }
}

impl ExcelInputIdentity for f64 {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.f64(*self);
    }
}

#[allow(
    unsafe_code,
    reason = "XLOPER12 boolean union projection is audited here"
)]
impl<'call> FromExcel<'call> for bool {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        if value.value_type() != XlValueType::Boolean {
            return Err(value.wrong_type(argument, "boolean"));
        }
        // SAFETY: XLTYPE_BOOL selects the boolean member.
        Ok(unsafe { value.raw.value.boolean } != 0)
    }
}

impl ExcelInputIdentity for bool {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.bool(*self);
    }
}

fn number_to_integer<T>(
    number: f64,
    argument: &'static str,
    minimum: f64,
    maximum: f64,
    convert: impl FnOnce(f64) -> T,
) -> XllResult<T> {
    if !number.is_finite() {
        return Err(XllError::input(argument, InputError::NonFinite));
    }
    if number.fract() != 0.0 {
        return Err(XllError::input(argument, InputError::NotInteger));
    }
    if number < minimum || number > maximum {
        return Err(XllError::input(argument, InputError::NumericOverflow));
    }
    Ok(convert(number))
}

#[allow(
    unsafe_code,
    reason = "XLOPER12 integer union projection is audited here"
)]
impl<'call> FromExcel<'call> for i32 {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.value_type() {
            // SAFETY: XLTYPE_INT selects the integer member.
            XlValueType::Integer => Ok(unsafe { value.raw.value.integer }),
            // SAFETY: XLTYPE_NUM selects the number member.
            XlValueType::Number => number_to_integer(
                unsafe { value.raw.value.number },
                argument,
                i32::MIN as f64,
                i32::MAX as f64,
                |number| number as i32,
            ),
            _ => Err(value.wrong_type(argument, "integer")),
        }
    }
}

impl ExcelInputIdentity for i32 {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.i64(i64::from(*self));
    }
}

#[allow(
    unsafe_code,
    reason = "XLOPER12 integer union projection is audited here"
)]
impl<'call> FromExcel<'call> for i64 {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.value_type() {
            // SAFETY: XLTYPE_INT selects the integer member.
            XlValueType::Integer => Ok((unsafe { value.raw.value.integer }) as i64),
            // Excel doubles can represent every integer only through 2^53.
            // SAFETY: XLTYPE_NUM selects the number member.
            XlValueType::Number => number_to_integer(
                unsafe { value.raw.value.number },
                argument,
                -((1_u64 << 53) as f64),
                (1_u64 << 53) as f64,
                |number| number as i64,
            ),
            _ => Err(value.wrong_type(argument, "integer")),
        }
    }
}

impl ExcelInputIdentity for i64 {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.i64(*self);
    }
}

impl<'call> FromExcel<'call> for String {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        String::from_utf16(value.utf16(argument)?)
            .map_err(|_| XllError::input(argument, InputError::InvalidUtf16))
    }
}

impl ExcelInputIdentity for String {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.string(self);
    }
}

impl<'call, M: InputMode> input::sealed::ExcelParameterSealed<'call, M> for &'call str {}

impl<'call, M: InputMode> ExcelParameter<'call, M> for &'call str {
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let text = context
            .scratch()
            .decode_utf16(value.utf16(argument)?, argument)?;
        M::string(identity, text);
        Ok(text)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        M::string(identity, self);
    }
}

#[allow(
    unsafe_code,
    reason = "XLOPER12 error union projection is audited here"
)]
impl<'call> FromExcel<'call> for ExcelErrorValue {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        if value.value_type() != XlValueType::Error {
            return Err(value.wrong_type(argument, "Excel error"));
        }
        // SAFETY: XLTYPE_ERR selects the error member.
        let code = unsafe { value.raw.value.error };
        ExcelError::from_code(code)
            .map(Self)
            .ok_or_else(|| XllError::input(argument, InputError::Malformed("unknown error code")))
    }
}

impl ExcelInputIdentity for ExcelErrorValue {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.i64(i64::from(self.0.code()));
    }
}

impl<'call> FromExcel<'call> for ExcelSerialDate {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        Self::new(
            <f64 as FromExcel>::from_excel(value, argument)?,
            ExcelDateSystem::Workbook,
        )
        .map_err(|error| match error {
            XllError::Input { reason, .. } => XllError::Input { argument, reason },
            other => other,
        })
    }
}

impl ExcelInputIdentity for ExcelSerialDate {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.f64(self.serial());
        encoder.tag(match self.date_system() {
            ExcelDateSystem::Workbook => 0,
            ExcelDateSystem::Windows1900 => 1,
            ExcelDateSystem::Mac1904 => 2,
        });
    }
}

fn convert_grid_elements<'call, T, M>(
    grid: &GridView<'call>,
    argument: &'static str,
    context: &CallContext<'call>,
    identity: &mut M::Identity,
) -> XllResult<Vec<T>>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    let (rows, columns) = grid.shape();
    let element_count = rows * columns;
    let output_bytes = element_count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("output byte-size overflow"))
        })?;
    let mut referenced_bytes = element_count
        .checked_mul(std::mem::size_of::<XLOPER12>())
        .and_then(|bytes| bytes.checked_add(output_bytes))
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("array byte-size overflow"))
        })?;
    if referenced_bytes > MAX_ARRAY_BYTES {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit: MAX_ARRAY_BYTES,
                actual: referenced_bytes,
            },
        ));
    }

    let mut data = Vec::with_capacity(element_count);
    for element in grid.cells().iter().map(XlValueRef::from_array_cell) {
        let element = element?;
        if element.value_type() == XlValueType::Multi {
            return Err(XllError::input(
                argument,
                InputError::Malformed("nested arrays are not supported"),
            ));
        }
        if element.value_type() == XlValueType::String {
            let string_bytes = element
                .utf16(argument)?
                .len()
                .checked_mul(std::mem::size_of::<u16>() + 3)
                .ok_or_else(|| {
                    XllError::input(
                        argument,
                        InputError::Malformed("array string byte-size overflow"),
                    )
                })?;
            referenced_bytes = referenced_bytes.checked_add(string_bytes).ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array byte-size overflow"))
            })?;
            if referenced_bytes > MAX_ARRAY_BYTES {
                return Err(XllError::input(
                    argument,
                    InputError::TooLarge {
                        limit: MAX_ARRAY_BYTES,
                        actual: referenced_bytes,
                    },
                ));
            }
        }
        let converted = T::decode(element, argument, context, identity)?;
        data.push(converted);
    }
    Ok(data)
}

fn convert_grid_elements_borrowed<'call, T, M>(
    grid: &GridView<'call>,
    argument: &'static str,
    context: &CallContext<'call>,
    identity: &mut M::Identity,
) -> XllResult<&'call [T]>
where
    M: InputMode,
    T: ExcelParameter<'call, M> + Copy,
{
    let (rows, columns) = grid.shape();
    let element_count = rows.checked_mul(columns).ok_or_else(|| {
        XllError::input(argument, InputError::Malformed("array dimension overflow"))
    })?;
    let output_bytes = element_count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("output byte-size overflow"))
        })?;
    let mut referenced_bytes = element_count
        .checked_mul(std::mem::size_of::<XLOPER12>())
        .and_then(|bytes| bytes.checked_add(output_bytes))
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("array byte-size overflow"))
        })?;
    if referenced_bytes > MAX_ARRAY_BYTES {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit: MAX_ARRAY_BYTES,
                actual: referenced_bytes,
            },
        ));
    }

    context.scratch().collect_copy(element_count, |index| {
        let element = XlValueRef::from_array_cell(&grid.cells()[index])?;
        if element.value_type() == XlValueType::Multi {
            return Err(XllError::input(
                argument,
                InputError::Malformed("nested arrays are not supported"),
            ));
        }
        if element.value_type() == XlValueType::String {
            let string_bytes = element
                .utf16(argument)?
                .len()
                .checked_mul(std::mem::size_of::<u16>() + 3)
                .ok_or_else(|| {
                    XllError::input(
                        argument,
                        InputError::Malformed("array string byte-size overflow"),
                    )
                })?;
            referenced_bytes = referenced_bytes.checked_add(string_bytes).ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array byte-size overflow"))
            })?;
            if referenced_bytes > MAX_ARRAY_BYTES {
                return Err(XllError::input(
                    argument,
                    InputError::TooLarge {
                        limit: MAX_ARRAY_BYTES,
                        actual: referenced_bytes,
                    },
                ));
            }
        }
        T::decode(element, argument, context, identity)
    })
}

impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for OptionalExcelValue<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
}

impl<'call, T, M> ExcelParameter<'call, M> for OptionalExcelValue<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        match value.value_type() {
            XlValueType::Missing => {
                M::tag(identity, 0);
                Ok(Self::Missing)
            }
            XlValueType::Nil => {
                M::tag(identity, 1);
                Ok(Self::Blank)
            }
            _ => {
                M::tag(identity, 2);
                T::decode(value, argument, context, identity).map(Self::Value)
            }
        }
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        match self {
            Self::Missing => M::tag(identity, 0),
            Self::Blank => M::tag(identity, 1),
            Self::Value(value) => {
                M::tag(identity, 2);
                T::encode_decoded(value, identity);
            }
        }
    }
}

impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for Option<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
}

impl<'call, T, M> ExcelParameter<'call, M> for Option<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        match value.value_type() {
            XlValueType::Missing | XlValueType::Nil => {
                M::bool(identity, false);
                Ok(None)
            }
            _ => {
                M::bool(identity, true);
                T::decode(value, argument, context, identity).map(Some)
            }
        }
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        match self {
            None => M::bool(identity, false),
            Some(value) => {
                M::bool(identity, true);
                T::encode_decoded(value, identity);
            }
        }
    }
}

impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for Matrix<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
}

impl<'call, T, M> ExcelParameter<'call, M> for Matrix<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, rows as u64);
        M::u64(identity, columns as u64);
        let data = convert_grid_elements::<T, M>(&grid, argument, context, identity)?;
        Matrix::new(rows, columns, data)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        M::u64(identity, self.rows() as u64);
        M::u64(identity, self.columns() as u64);
        for value in self.as_slice() {
            T::encode_decoded(value, identity);
        }
    }
}

impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for MatrixRef<'call, T>
where
    M: InputMode,
    T: ExcelParameter<'call, M> + Copy,
{
}

impl<'call, T, M> ExcelParameter<'call, M> for MatrixRef<'call, T>
where
    M: InputMode,
    T: ExcelParameter<'call, M> + Copy,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, rows as u64);
        M::u64(identity, columns as u64);
        let data = convert_grid_elements_borrowed::<T, M>(&grid, argument, context, identity)?;
        MatrixRef::from_slice(rows, columns, data)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        M::u64(identity, self.rows() as u64);
        M::u64(identity, self.columns() as u64);
        for value in self.as_slice() {
            T::encode_decoded(value, identity);
        }
    }
}

impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for Vec<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
}

impl<'call, T, M> ExcelParameter<'call, M> for Vec<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if rows != 1 && columns != 1 {
            return Err(XllError::Shape {
                expected: Shape {
                    rows: 1,
                    columns: rows * columns,
                },
                actual: Shape { rows, columns },
            });
        }
        M::u64(identity, (rows * columns) as u64);
        convert_grid_elements::<T, M>(&grid, argument, context, identity)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        M::u64(identity, self.len() as u64);
        for value in self {
            T::encode_decoded(value, identity);
        }
    }
}

impl<'call, T, M, const MAX: usize> input::sealed::ExcelParameterSealed<'call, M>
    for BoundedVarArgs<T, MAX>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
}

impl<'call, T, M, const MAX: usize> ExcelParameter<'call, M> for BoundedVarArgs<T, MAX>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        if MAX == 0 {
            return Err(XllError::input(
                argument,
                InputError::Malformed("bounded varargs maximum must be non-zero"),
            ));
        }
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if rows != 1 && columns != 1 {
            return Err(XllError::Shape {
                expected: Shape {
                    rows: 1,
                    columns: rows * columns,
                },
                actual: Shape { rows, columns },
            });
        }
        let actual = rows * columns;
        if actual > MAX {
            return Err(XllError::input(
                argument,
                InputError::TooLarge { limit: MAX, actual },
            ));
        }
        M::u64(identity, actual as u64);
        let elements = convert_grid_elements::<T, M>(&grid, argument, context, identity)?;
        Self::new(elements).map_err(|error| match error {
            XllError::Input { reason, .. } => XllError::Input { argument, reason },
            other => other,
        })
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        M::u64(identity, self.as_slice().len() as u64);
        for value in self.as_slice() {
            T::encode_decoded(value, identity);
        }
    }
}

impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for Row<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
}

impl<'call, T, M> ExcelParameter<'call, M> for Row<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if rows != 1 {
            return Err(XllError::Shape {
                expected: Shape { rows: 1, columns },
                actual: Shape { rows, columns },
            });
        }
        M::u64(identity, columns as u64);
        convert_grid_elements::<T, M>(&grid, argument, context, identity).map(Self)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        M::u64(identity, self.as_slice().len() as u64);
        for value in self.as_slice() {
            T::encode_decoded(value, identity);
        }
    }
}

impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for Column<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
}

impl<'call, T, M> ExcelParameter<'call, M> for Column<T>
where
    M: InputMode,
    T: ExcelParameter<'call, M>,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        if columns != 1 {
            return Err(XllError::Shape {
                expected: Shape { rows, columns: 1 },
                actual: Shape { rows, columns },
            });
        }
        M::u64(identity, rows as u64);
        convert_grid_elements::<T, M>(&grid, argument, context, identity).map(Self)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        M::u64(identity, self.as_slice().len() as u64);
        for value in self.as_slice() {
            T::encode_decoded(value, identity);
        }
    }
}

impl<'call> FromExcel<'call> for ExcelCellValue {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.value_type() {
            XlValueType::Number | XlValueType::Integer => {
                <f64 as FromExcel>::from_excel(value, argument).map(Self::Number)
            }
            XlValueType::Boolean => {
                <bool as FromExcel>::from_excel(value, argument).map(Self::Boolean)
            }
            XlValueType::String => String::from_excel(value, argument).map(Self::String),
            XlValueType::Error => {
                ExcelErrorValue::from_excel(value, argument).map(|value| Self::Error(value.0))
            }
            XlValueType::Nil => Ok(Self::Blank),
            _ => Err(value.wrong_type(argument, "worksheet value")),
        }
    }
}

impl ExcelInputIdentity for ExcelCellValue {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            Self::Number(value) => {
                encoder.tag(1);
                value.encode_input_identity(encoder);
            }
            Self::Boolean(value) => {
                encoder.tag(2);
                value.encode_input_identity(encoder);
            }
            Self::String(value) => {
                encoder.tag(3);
                value.encode_input_identity(encoder);
            }
            Self::Error(value) => {
                encoder.tag(4);
                encoder.i64(i64::from(value.code()));
            }
            Self::Blank => encoder.tag(5),
        }
    }
}

impl<'call, M: InputMode> input::sealed::ExcelParameterSealed<'call, M> for ExcelCellRef<'call> {}

impl<'call, M: InputMode> ExcelParameter<'call, M> for ExcelCellRef<'call> {
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        match value.value_type() {
            XlValueType::Number | XlValueType::Integer => {
                let number = <f64 as FromExcel>::from_excel(value, argument)?;
                M::f64(identity, number);
                Ok(Self::Number(number))
            }
            XlValueType::Boolean => {
                let boolean = <bool as FromExcel>::from_excel(value, argument)?;
                M::bool(identity, boolean);
                Ok(Self::Boolean(boolean))
            }
            XlValueType::String => {
                let text = context
                    .scratch()
                    .decode_utf16(value.utf16(argument)?, argument)?;
                M::string(identity, text);
                Ok(Self::String(text))
            }
            XlValueType::Error => {
                let error = <ExcelErrorValue as FromExcel>::from_excel(value, argument)?.0;
                M::i64(identity, i64::from(error.code()));
                Ok(Self::Error(error))
            }
            XlValueType::Nil => {
                M::tag(identity, 5);
                Ok(Self::Blank)
            }
            _ => Err(value.wrong_type(argument, "worksheet cell")),
        }
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        match self {
            Self::Number(value) => {
                M::tag(identity, 1);
                M::f64(identity, *value);
            }
            Self::Boolean(value) => {
                M::tag(identity, 2);
                M::bool(identity, *value);
            }
            Self::String(value) => {
                M::tag(identity, 3);
                M::string(identity, value);
            }
            Self::Error(value) => {
                M::tag(identity, 4);
                M::i64(identity, i64::from(value.code()));
            }
            Self::Blank => M::tag(identity, 5),
        }
    }
}

impl<'call> ExcelInputIdentity for ExcelCellRef<'call> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            Self::Number(value) => {
                encoder.tag(1);
                encoder.f64(*value);
            }
            Self::Boolean(value) => {
                encoder.tag(2);
                encoder.bool(*value);
            }
            Self::String(value) => {
                encoder.tag(3);
                encoder.string(value);
            }
            Self::Error(value) => {
                encoder.tag(4);
                encoder.i64(i64::from(value.code()));
            }
            Self::Blank => encoder.tag(5),
        }
    }
}

pub(crate) fn decode_owned_matrix<'call, T>(
    value: XlValueRef<'call>,
    argument: &'static str,
) -> XllResult<Matrix<T>>
where
    T: FromExcel<'call>,
{
    let grid = GridView::from_value(value, argument)?;
    let (rows, columns) = grid.shape();
    convert_owned_grid_elements(&grid, argument).and_then(|data| Matrix::new(rows, columns, data))
}

fn convert_owned_grid_elements<'call, T>(
    grid: &GridView<'call>,
    argument: &'static str,
) -> XllResult<Vec<T>>
where
    T: FromExcel<'call>,
{
    let (rows, columns) = grid.shape();
    let element_count = rows.checked_mul(columns).ok_or_else(|| {
        XllError::input(argument, InputError::Malformed("array dimension overflow"))
    })?;
    let output_bytes = element_count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("output byte-size overflow"))
        })?;
    let mut referenced_bytes = element_count
        .checked_mul(std::mem::size_of::<XLOPER12>())
        .and_then(|bytes| bytes.checked_add(output_bytes))
        .ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("array byte-size overflow"))
        })?;
    if referenced_bytes > MAX_ARRAY_BYTES {
        return Err(XllError::input(
            argument,
            InputError::TooLarge {
                limit: MAX_ARRAY_BYTES,
                actual: referenced_bytes,
            },
        ));
    }

    let mut data = Vec::with_capacity(element_count);
    for element in grid.cells().iter().map(XlValueRef::from_array_cell) {
        let element = element?;
        if element.value_type() == XlValueType::Multi {
            return Err(XllError::input(
                argument,
                InputError::Malformed("nested arrays are not supported"),
            ));
        }
        if element.value_type() == XlValueType::String {
            let string_bytes = element
                .utf16(argument)?
                .len()
                .checked_mul(std::mem::size_of::<u16>() + 3)
                .ok_or_else(|| {
                    XllError::input(
                        argument,
                        InputError::Malformed("array string byte-size overflow"),
                    )
                })?;
            referenced_bytes = referenced_bytes.checked_add(string_bytes).ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array byte-size overflow"))
            })?;
            if referenced_bytes > MAX_ARRAY_BYTES {
                return Err(XllError::input(
                    argument,
                    InputError::TooLarge {
                        limit: MAX_ARRAY_BYTES,
                        actual: referenced_bytes,
                    },
                ));
            }
        }
        data.push(T::from_excel(element, argument)?);
    }
    Ok(data)
}

impl<'call> FromExcel<'call> for ExcelValue {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.value_type() {
            XlValueType::Missing => Ok(Self::Missing),
            XlValueType::Multi => {
                decode_owned_matrix::<ExcelCellValue>(value, argument).map(Self::Array)
            }
            _ => ExcelCellValue::from_excel(value, argument).map(Self::Scalar),
        }
    }
}

impl ExcelInputIdentity for ExcelValue {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        match self {
            Self::Scalar(value) => {
                encoder.tag(1);
                value.encode_input_identity(encoder);
            }
            Self::Missing => encoder.tag(2),
            Self::Array(value) => {
                encoder.tag(3);
                encoder.u64(value.rows() as u64);
                encoder.u64(value.columns() as u64);
                for cell in value.as_slice() {
                    cell.encode_input_identity(encoder);
                }
            }
        }
    }
}

impl<'call> ExcelInputIdentity for XlArrayRef<'call> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.rows() as u64);
        encoder.u64(self.columns() as u64);
        for cell in self.cells() {
            encode_raw_value(cell, true, encoder);
        }
    }
}

#[cfg(feature = "handles")]
impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for crate::handle::Handle<'call, T>
where
    M: InputMode,
    T: crate::handle::ExcelHandleObject,
{
}

#[cfg(feature = "handles")]
impl<'call, T, M> ExcelParameter<'call, M> for crate::handle::Handle<'call, T>
where
    M: InputMode,
    T: crate::handle::ExcelHandleObject,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let token = context
            .scratch()
            .decode_utf16(value.utf16(argument)?, argument)?;
        let handle = context.resolve_handle::<T>(token)?;
        let object_id = handle.object_id();
        M::u64(identity, object_id.session());
        M::u64(identity, object_id.sequence());
        Ok(handle)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        let object_id = self.object_id();
        M::u64(identity, object_id.session());
        M::u64(identity, object_id.sequence());
    }
}

#[cfg(feature = "handles")]
impl<'call, T, M> input::sealed::ExcelParameterSealed<'call, M> for crate::handle::HandleLease<T>
where
    M: InputMode,
    T: crate::handle::ExcelHandleObject,
{
}

#[cfg(feature = "handles")]
impl<'call, T, M> ExcelParameter<'call, M> for crate::handle::HandleLease<T>
where
    M: InputMode,
    T: crate::handle::ExcelHandleObject,
{
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let token = context
            .scratch()
            .decode_utf16(value.utf16(argument)?, argument)?;
        let handle = context.resolve_handle::<T>(token)?.pin()?;
        let object_id = handle.object_id();
        M::u64(identity, object_id.session());
        M::u64(identity, object_id.sequence());
        Ok(handle)
    }

    fn encode_decoded(&self, identity: &mut M::Identity) {
        let object_id = self.object_id();
        M::u64(identity, object_id.session());
        M::u64(identity, object_id.sequence());
    }
}

impl IntoExcel for ExcelCellOutput {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        if matches!(self, Self::Number(value) if !value.is_finite()) {
            return Err(XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            });
        }
        Ok(self)
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_cell(self)
    }
}

impl IntoExcel for f64 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        if self.is_finite() {
            Ok(ExcelCellOutput::Number(self))
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            })
        }
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_f64(self)
    }
}

impl IntoExcel for bool {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::Boolean(self))
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_bool(self)
    }
}

impl IntoExcel for i32 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::Number(self as f64))
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_f64(self as f64)
    }
}

impl IntoExcel for i64 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        const EXACT_LIMIT: i64 = 1_i64 << 53;
        if (-EXACT_LIMIT..=EXACT_LIMIT).contains(&self) {
            Ok(ExcelCellOutput::Number(self as f64))
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
        }
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        const EXACT_LIMIT: i64 = 1_i64 << 53;
        if (-EXACT_LIMIT..=EXACT_LIMIT).contains(&self) {
            sink.push_f64(self as f64)
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
        }
    }
}

impl IntoExcel for ExcelSerialDate {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        IntoExcel::into_excel(self.serial)
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_f64(self.serial)
    }
}

impl IntoExcel for String {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::String(self))
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_string(self)
    }
}

impl IntoExcel for &str {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::String(self.to_owned()))
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_string(self.to_owned())
    }
}

impl IntoExcel for ExcelErrorValue {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::Error(self.0))
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_error(self.0)
    }
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "Value tests exercise the audited raw input boundary"
)]
mod tests {
    use super::*;
    use crate::call_return::{
        AsyncReturn, ExcelReturn, MacroSheetReturn, MainThreadReturn, ReturnContext, ReturnPayload,
        ThreadSafeReturn, VolatileReturn,
    };
    use crate::return_abi::XlArrayBuilder;
    use proptest::prelude::*;
    use static_assertions::assert_impl_all;
    use xlfn_sys::{XLBIT_XL_FREE, XLOPER12Value, XLTYPE_MULTI, XLTYPE_STR};

    assert_impl_all!(ExcelValue: std::panic::UnwindSafe, std::panic::RefUnwindSafe);
    assert_impl_all!(
        XlArrayBuilder: std::panic::UnwindSafe, std::panic::RefUnwindSafe
    );

    fn convert<T>(raw: &mut XLOPER12) -> XllResult<T>
    where
        T: for<'call> ExcelParameter<'call, PlainInputMode>,
    {
        // SAFETY: raw is live for this conversion.
        with_excel_call_scope(|scope| unsafe { argument_from_raw(scope, "arg", raw) })
    }

    fn convert_with_identity<T>(
        raw: &mut XLOPER12,
    ) -> XllResult<(T, crate::input_identity::InputFingerprint)>
    where
        T: for<'call> ExcelParameter<'call, FormulaInputMode>,
    {
        with_excel_call_scope(|scope| {
            let mut builder = crate::input_identity::InputFingerprintBuilder::new(1);
            // SAFETY: raw is live for this conversion.
            let value = unsafe {
                let value_ref = XlValueRef::from_raw(raw)?;
                let mut converted = None;
                builder.with_argument(0, "arg", |encoder| {
                    converted = Some(T::decode(
                        value_ref,
                        "arg",
                        &CallContext::plain(scope),
                        encoder,
                    )?);
                    Ok(())
                })?;
                converted.expect("formula conversion must produce a value")
            };
            let fingerprint = builder.finish()?;
            Ok((value, fingerprint))
        })
    }

    fn raw_array_identity(value: XlArrayRef<'_>) -> crate::input_identity::InputFingerprint {
        let mut builder = crate::input_identity::InputFingerprintBuilder::new(1);
        builder
            .with_argument(0, "arg", |encoder| {
                encoder.u64(value.rows() as u64);
                encoder.u64(value.columns() as u64);
                for cell in value.cells() {
                    encode_raw_value(cell, true, encoder);
                }
                Ok(())
            })
            .unwrap();
        builder.finish().unwrap()
    }

    #[test]
    fn integer_conversion_checks_fraction_and_range() {
        let mut fractional = XLOPER12::number(1.5);
        assert!(matches!(
            convert::<i32>(&mut fractional),
            Err(XllError::Input {
                reason: InputError::NotInteger,
                ..
            })
        ));

        let mut huge = XLOPER12::number(i32::MAX as f64 + 1.0);
        assert!(matches!(
            convert::<i32>(&mut huge),
            Err(XllError::Input {
                reason: InputError::NumericOverflow,
                ..
            })
        ));
    }

    #[test]
    fn integer_identity_uses_the_converted_value() {
        let mut integer = XLOPER12::integer(1);
        let mut number = XLOPER12::number(1.0);
        let (_, integer_identity) = convert_with_identity::<i32>(&mut integer).unwrap();
        let (_, number_identity) = convert_with_identity::<i32>(&mut number).unwrap();
        assert_eq!(integer_identity, number_identity);
    }

    #[test]
    fn default_identity_matches_explicit_semantic_values() {
        crate::call::with_excel_call_scope(|scope| {
            let mut defaults = ArgumentContext::<FormulaInputMode>::from_scope(scope, 2);
            defaults.record_decoded(0, "first", &0.0_f64).unwrap();
            defaults.record_decoded(1, "second", &1.0_f64).unwrap();
            let default_identity = defaults.finish().unwrap().unwrap();

            let mut first = XLOPER12::number(0.0);
            let mut second = XLOPER12::number(1.0);
            let mut explicit = ArgumentContext::<FormulaInputMode>::from_scope(scope, 2);
            // SAFETY: first raw value remains live for this call.
            unsafe {
                argument_from_raw_with_arguments::<FormulaInputMode, f64>(
                    &mut explicit,
                    0,
                    "first",
                    &mut first,
                )
                .unwrap();
            }
            // SAFETY: second raw value remains live for this call.
            unsafe {
                argument_from_raw_with_arguments::<FormulaInputMode, f64>(
                    &mut explicit,
                    1,
                    "second",
                    &mut second,
                )
                .unwrap();
            }
            let explicit_identity = explicit.finish().unwrap().unwrap();
            assert_eq!(default_identity, explicit_identity);
        });
    }

    proptest! {
        #[test]
        fn integer_values_round_trip_through_excel_storage(value in any::<i32>()) {
            let mut raw = XLOPER12::integer(value);
            prop_assert_eq!(convert::<i32>(&mut raw).unwrap(), value);
        }
    }

    #[test]
    fn missing_and_blank_remain_distinct() {
        let mut missing = XLOPER12::missing();
        let mut blank = XLOPER12::nil();
        assert_eq!(
            convert::<OptionalExcelValue<f64>>(&mut missing).unwrap(),
            OptionalExcelValue::Missing
        );
        assert_eq!(
            convert::<OptionalExcelValue<f64>>(&mut blank).unwrap(),
            OptionalExcelValue::Blank
        );
    }

    #[test]
    fn return_trait_resolves_result_aliases_without_name_matching() {
        type AliasedReturn = Result<f64, XllError>;
        let mut context = ReturnContext::new();
        let value =
            <AliasedReturn as ExcelReturn>::into_excel(Ok::<_, XllError>(4.5), &mut context)
                .unwrap();
        assert!(matches!(
            value,
            ReturnPayload::Scalar(ExcelCellOutput::Number(number)) if number == 4.5
        ));
    }

    #[test]
    fn result_and_collection_returns_forward_all_standard_modes() {
        fn assert_modes<T>()
        where
            T: MainThreadReturn
                + ThreadSafeReturn
                + MacroSheetReturn
                + AsyncReturn
                + VolatileReturn,
        {
        }

        assert_modes::<f64>();
        assert_modes::<Result<f64, XllError>>();
        assert_modes::<Matrix<f64>>();
        assert_modes::<crate::subscription::RtdValue>();
    }

    #[test]
    fn serial_date_keeps_workbook_system_unresolved() {
        let mut raw = XLOPER12::number(60.25);
        let date: ExcelSerialDate = convert(&mut raw).unwrap();
        assert_eq!(date.serial(), 60.25);
        assert_eq!(date.date_system(), ExcelDateSystem::Workbook);
        let date = date.with_date_system(ExcelDateSystem::Windows1900);
        assert!(date.is_fictitious_1900_leap_day());
        assert_eq!(date.fractional_day(), 0.25);
    }

    #[test]
    fn matrix_column_is_checked() {
        let matrix = Matrix::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(
            matrix.column(1).unwrap().copied().collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert!(matrix.column(2).is_none());
    }

    #[test]
    fn matrix_index_rejects_each_out_of_bounds_dimension_before_flattening() {
        let matrix = Matrix::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(matrix[(1, 1)], 4);
        assert!(
            std::panic::catch_unwind(|| matrix[(usize::MAX, 2)]).is_err(),
            "overflowing coordinates must not wrap onto a valid element"
        );
        assert!(std::panic::catch_unwind(|| matrix[(0, 2)]).is_err());
        assert!(std::panic::catch_unwind(|| matrix[(2, 0)]).is_err());
    }

    #[test]
    fn strict_utf16_rejects_unpaired_surrogate() {
        let mut text = vec![1_u16, 0xd800];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR | XLBIT_XL_FREE,
        };
        assert!(matches!(
            convert::<String>(&mut raw),
            Err(XllError::Input {
                reason: InputError::InvalidUtf16,
                ..
            })
        ));
    }

    #[test]
    fn borrowed_string_is_decoded_into_the_call_scratch() {
        let text: Vec<u16> = std::iter::once(5_u16)
            .chain("日本語💡".encode_utf16())
            .collect();
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_ptr().cast_mut(),
            },
            xltype: XLTYPE_STR,
        };

        with_excel_call_scope(|scope| {
            // SAFETY: raw and its UTF-16 payload remain live for this scope.
            let value: &str = unsafe { argument_from_raw(scope, "text", &mut raw) }.unwrap();
            assert_eq!(value, "日本語💡");
        });
    }

    #[test]
    fn borrowed_matrix_and_cell_views_preserve_shape_and_strings() {
        let mut first = vec![3_u16, '猫' as u16, 'A' as u16, 'B' as u16];
        let mut second = vec![3_u16, '犬' as u16, 'C' as u16, 'D' as u16];
        let mut cells = [
            XLOPER12 {
                value: XLOPER12Value {
                    string: first.as_mut_ptr(),
                },
                xltype: XLTYPE_STR,
            },
            XLOPER12 {
                value: XLOPER12Value {
                    string: second.as_mut_ptr(),
                },
                xltype: XLTYPE_STR,
            },
        ];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: cells.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        with_excel_call_scope(|scope| {
            // SAFETY: raw, cells, and both UTF-16 payloads remain live for this scope.
            let values: MatrixRef<'_, &str> =
                unsafe { argument_from_raw(scope, "values", &mut raw) }.unwrap();
            assert_eq!((values.rows(), values.columns()), (1, 2));
            assert_eq!(values.as_slice(), &["猫AB", "犬CD"]);

            let mut number = XLOPER12::number(4.0);
            // SAFETY: number remains live for the duration of this scope.
            let cell: ExcelCellRef<'_> =
                unsafe { argument_from_raw(scope, "cell", &mut number) }.unwrap();
            assert_eq!(cell, ExcelCellRef::Number(4.0));

            let mut mixed_text = vec![3_u16, '猫' as u16, 'A' as u16, 'B' as u16];
            let mut mixed_cells = [
                XLOPER12::number(2.0),
                XLOPER12 {
                    value: XLOPER12Value {
                        string: mixed_text.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                },
                XLOPER12::nil(),
            ];
            let mut mixed_raw = XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        values: mixed_cells.as_mut_ptr(),
                        rows: 1,
                        columns: 3,
                    },
                },
                xltype: XLTYPE_MULTI,
            };
            // SAFETY: the mixed array and its string payload remain live for this scope.
            let mixed: MatrixRef<'_, ExcelCellRef<'_>> =
                unsafe { argument_from_raw(scope, "mixed", &mut mixed_raw) }.unwrap();
            assert_eq!(
                mixed.as_slice(),
                &[
                    ExcelCellRef::Number(2.0),
                    ExcelCellRef::String("猫AB"),
                    ExcelCellRef::Blank,
                ]
            );
        });
    }

    #[test]
    fn borrowed_string_reports_the_named_argument_when_decoding_is_deferred() {
        let mut text = vec![1_u16, 0xd800];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR | XLBIT_XL_FREE,
        };

        with_excel_call_scope(|_| {
            // SAFETY: raw and its UTF-16 payload remain live for this scope.
            let value = unsafe { XlValueRef::from_raw(&mut raw) }.unwrap();
            let string = value.as_str_with_argument("currency").unwrap();
            assert!(matches!(
                string.to_string(),
                Err(XllError::Input {
                    argument: "currency",
                    reason: InputError::InvalidUtf16,
                })
            ));
        });
    }

    #[test]
    fn matrix_is_read_in_row_major_order() {
        let mut elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
            XLOPER12::number(4.0),
        ];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 2,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let matrix = convert::<Matrix<f64>>(&mut raw).unwrap();
        assert_eq!(matrix.rows(), 2);
        assert_eq!(matrix.columns(), 2);
        assert_eq!(matrix.as_slice(), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn scalar_values_lift_to_one_by_one_collections() {
        let mut number = XLOPER12::number(7.0);
        let matrix = convert::<Matrix<f64>>(&mut number).unwrap();
        assert_eq!((matrix.rows(), matrix.columns()), (1, 1));
        assert_eq!(matrix.as_slice(), &[7.0]);

        let mut number = XLOPER12::number(7.0);
        assert_eq!(convert::<Row<f64>>(&mut number).unwrap().as_slice(), &[7.0]);
        let mut number = XLOPER12::number(7.0);
        assert_eq!(
            convert::<Column<f64>>(&mut number).unwrap().as_slice(),
            &[7.0]
        );
        let mut number = XLOPER12::number(7.0);
        assert_eq!(convert::<Vec<f64>>(&mut number).unwrap(), vec![7.0]);
    }

    #[test]
    fn bounded_varargs_enforce_the_type_level_limit() {
        let mut elements = vec![XLOPER12::number(1.0), XLOPER12::number(2.0)];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        assert_eq!(
            convert::<BoundedVarArgs<f64, 2>>(&mut raw)
                .unwrap()
                .as_slice(),
            &[1.0, 2.0]
        );
        assert!(matches!(
            convert::<BoundedVarArgs<f64, 1>>(&mut raw),
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 1,
                    actual: 2
                },
                ..
            })
        ));
    }

    #[test]
    fn bounded_varargs_rejects_oversized_input_before_converting_elements() {
        struct PanicOnConvert;
        impl<'call> FromExcel<'call> for PanicOnConvert {
            fn from_excel(_value: XlValueRef<'call>, _argument: &'static str) -> XllResult<Self> {
                panic!("element conversion should not occur for oversized inputs");
            }
        }

        let mut elements = vec![XLOPER12::number(1.0), XLOPER12::number(2.0)];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        let result = convert::<BoundedVarArgs<PanicOnConvert, 1>>(&mut raw);
        assert!(matches!(
            result,
            Err(XllError::Input {
                reason: InputError::TooLarge {
                    limit: 1,
                    actual: 2
                },
                ..
            })
        ));
    }

    #[test]
    fn blank_and_error_elements_keep_existing_conversion_rules() {
        let mut elements = vec![XLOPER12::nil(), XLOPER12::error(xlfn_sys::XLERR_NA)];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let values = convert::<Matrix<ExcelCellValue>>(&mut raw).unwrap();
        assert_eq!(values.as_slice()[0], ExcelCellValue::Blank);
        assert_eq!(
            values.as_slice()[1],
            ExcelCellValue::Error(ExcelError::NotAvailable)
        );
    }

    #[test]
    fn dynamic_values_separate_missing_from_blank_and_canonicalize_integers() {
        let mut missing = XLOPER12::missing();
        assert_eq!(
            convert::<ExcelValue>(&mut missing).unwrap(),
            ExcelValue::Missing
        );

        let mut blank = XLOPER12::nil();
        assert_eq!(
            convert::<ExcelValue>(&mut blank).unwrap(),
            ExcelValue::Scalar(ExcelCellValue::Blank)
        );

        let mut integer = XLOPER12::integer(7);
        assert_eq!(
            convert::<ExcelValue>(&mut integer).unwrap(),
            ExcelValue::Scalar(ExcelCellValue::Number(7.0))
        );

        let mut cells = [XLOPER12::nil(), XLOPER12::integer(8)];
        let mut array = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: cells.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        assert_eq!(
            convert::<ExcelValue>(&mut array).unwrap(),
            ExcelValue::Array(
                Matrix::new(
                    1,
                    2,
                    vec![ExcelCellValue::Blank, ExcelCellValue::Number(8.0)],
                )
                .unwrap(),
            )
        );
    }

    #[test]
    fn non_finite_values_are_rejected_both_directions() {
        let mut raw = XLOPER12::number(f64::NAN);
        assert!(convert::<f64>(&mut raw).is_err());
        assert!(IntoExcel::into_excel(f64::INFINITY).is_err());
    }

    #[test]
    fn typed_arguments_propagate_excel_error_values() {
        let mut raw = XLOPER12::error(xlfn_sys::XLERR_NA);
        let error = convert::<f64>(&mut raw).unwrap_err();
        assert_eq!(error.excel_error(), ExcelError::NotAvailable);
    }

    #[test]
    fn malformed_xltype_flags_are_rejected() {
        let mut raw = XLOPER12::number(1.0);
        raw.xltype |= 0x2000;
        assert!(matches!(
            convert::<f64>(&mut raw),
            Err(XllError::Input {
                reason: InputError::Malformed("unknown xltype flag"),
                ..
            })
        ));
    }

    #[test]
    fn custom_conversion_can_return_owned_data() {
        #[derive(Debug, PartialEq)]
        struct FiniteNumber(f64);

        impl<'call> FromExcel<'call> for FiniteNumber {
            fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
                <f64 as FromExcel>::from_excel(value, argument).map(Self)
            }
        }

        let mut raw = XLOPER12::number(42.0);
        assert_eq!(
            convert::<FiniteNumber>(&mut raw).unwrap(),
            FiniteNumber(42.0)
        );
    }

    #[test]
    fn borrowed_array_reads_cells_without_materializing_them() {
        let mut elements = [
            XLOPER12::number(1.5),
            XLOPER12::integer(2),
            XLOPER12::boolean(true),
            XLOPER12::nil(),
        ];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: elements.as_mut_ptr(),
                    rows: 2,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        with_excel_call_scope(|scope| {
            // SAFETY: raw and its four cells remain live inside this scope.
            let view: XlArrayRef<'_> =
                unsafe { argument_from_raw(scope, "values", &mut raw) }.unwrap();
            assert_eq!(view.shape(), (2, 2));
            assert_eq!(view.get(0, 0).unwrap().as_f64().unwrap(), 1.5);
            assert_eq!(view.get(0, 1).unwrap().as_f64().unwrap(), 2.0);
            assert!(view.get(1, 0).unwrap().as_bool().unwrap());
            assert!(view.get(1, 1).unwrap().is_blank());
        });
    }

    #[test]
    fn raw_array_views_preserve_raw_numeric_bits() {
        let mut negative_cell = [XLOPER12::number(-0.0)];
        let mut positive_cell = [XLOPER12::number(0.0)];
        let mut negative = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: negative_cell.as_mut_ptr(),
                    rows: 1,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let mut positive = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: positive_cell.as_mut_ptr(),
                    rows: 1,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        with_excel_call_scope(|scope| {
            // SAFETY: both arrays and their cells remain live for this scope.
            let negative_view: XlArrayRef<'_> =
                unsafe { argument_from_raw(scope, "negative", &mut negative) }.unwrap();
            // SAFETY: both arrays and their cells remain live for this scope.
            let positive_view: XlArrayRef<'_> =
                unsafe { argument_from_raw(scope, "positive", &mut positive) }.unwrap();
            assert_ne!(
                raw_array_identity(negative_view),
                raw_array_identity(positive_view)
            );
        });
    }

    #[test]
    fn borrowed_array_rejects_a_misaligned_cell_buffer() {
        let mut storage = [XLOPER12::nil(), XLOPER12::nil()];
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    // Deliberately misaligned; validation must reject it before reading.
                    values: storage.as_mut_ptr().cast::<u8>().wrapping_add(1).cast(),
                    rows: 1,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        with_excel_call_scope(|scope| {
            // SAFETY: the root is live; the malformed nested pointer is tested for rejection.
            let result = unsafe { argument_from_raw::<XlArrayRef<'_>>(scope, "values", &mut raw) };
            assert!(matches!(
                result,
                Err(XllError::Input {
                    reason: InputError::Malformed("misaligned array pointer"),
                    ..
                })
            ));
        });
    }

    #[test]
    fn array_builder_encodes_directly_into_its_finished_cell_buffer() {
        let mut builder = XlArrayBuilder::new(2, 2).unwrap();
        for value in [1.0, 2.0, 3.0, 4.0] {
            builder.push_f64(value).unwrap();
        }
        let encoded = builder.finish().unwrap();
        assert_eq!((encoded.rows, encoded.columns), (2, 2));
        assert_eq!(encoded.cells.len(), 4);
        for (cell, expected) in encoded.cells.iter().zip([1.0, 2.0, 3.0, 4.0]) {
            assert_eq!(cell.base_type(), xlfn_sys::XLTYPE_NUM);
            // SAFETY: XLTYPE_NUM selects the number member.
            assert_eq!(unsafe { cell.value.number }, expected);
        }
    }

    #[test]
    fn matrix_dimensions_must_fit_a_non_empty_worksheet_shape() {
        assert!(Matrix::<f64>::new(0, 1, Vec::new()).is_err());
        assert!(Matrix::<f64>::new(1, 0, Vec::new()).is_err());
        assert!(Matrix::<f64>::new(EXCEL_MAX_ROWS + 1, 1, Vec::new()).is_err());
        assert!(Matrix::<f64>::new(1, EXCEL_MAX_COLUMNS + 1, Vec::new()).is_err());
    }

    #[test]
    fn oversized_excel_dimensions_are_rejected_before_element_access() {
        for (rows, columns, limit, actual) in [
            (
                i32::try_from(EXCEL_MAX_ROWS + 1).unwrap(),
                1,
                EXCEL_MAX_ROWS,
                EXCEL_MAX_ROWS + 1,
            ),
            (
                1,
                i32::try_from(EXCEL_MAX_COLUMNS + 1).unwrap(),
                EXCEL_MAX_COLUMNS,
                EXCEL_MAX_COLUMNS + 1,
            ),
        ] {
            let mut raw = XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        values: std::ptr::null_mut(),
                        rows,
                        columns,
                    },
                },
                xltype: XLTYPE_MULTI,
            };

            assert!(matches!(
                convert::<Matrix<f64>>(&mut raw),
                Err(XllError::Input {
                    reason: InputError::TooLarge {
                        limit: error_limit,
                        actual: error_actual,
                    },
                    ..
                }) if error_limit == limit && error_actual == actual
            ));
        }
    }

    #[test]
    fn matrix_number_return_uses_encoded_array_output() {
        let matrix = Matrix::new(1, 2, vec![1.0, 2.0]).unwrap();
        let value =
            <Matrix<f64> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new()).unwrap();
        assert!(matches!(value, ReturnPayload::Array(_)));
    }

    #[test]
    fn element_conversion_is_called_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountedCell<'a> {
            conversions: &'a AtomicUsize,
            value: f64,
        }

        impl IntoExcel for CountedCell<'_> {
            fn into_excel(self) -> XllResult<ExcelCellOutput> {
                self.conversions.fetch_add(1, Ordering::Relaxed);
                IntoExcel::into_excel(self.value)
            }
        }

        let conversions = AtomicUsize::new(0);
        let data: Vec<_> = (0..1000)
            .map(|i| CountedCell {
                conversions: &conversions,
                value: i as f64,
            })
            .collect();
        let matrix = Matrix::new(10, 100, data).unwrap();
        let _value =
            <Matrix<CountedCell<'_>> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new())
                .unwrap();
        assert_eq!(conversions.load(Ordering::Relaxed), 1000);
    }

    #[test]
    fn partial_failure_during_matrix_conversion_cleans_up_safely() {
        let data = vec![1.0, 2.0, f64::NAN, 4.0];
        let matrix = Matrix::new(2, 2, data).unwrap();
        let result = <Matrix<f64> as ExcelReturn>::into_excel(matrix, &mut ReturnContext::new());
        assert!(result.is_err());
    }

    #[test]
    fn f64_semantic_identity_canonicalizes_integer_representation() {
        let mut int_raw = XLOPER12::integer(1);
        let mut num_raw = XLOPER12::number(1.0);
        let (int_val, int_id) = convert_with_identity::<f64>(&mut int_raw).unwrap();
        let (num_val, num_id) = convert_with_identity::<f64>(&mut num_raw).unwrap();
        assert_eq!(int_val, 1.0);
        assert_eq!(num_val, 1.0);
        assert_eq!(int_id, num_id);

        let mut pos_zero = XLOPER12::number(0.0);
        let mut neg_zero = XLOPER12::number(-0.0);
        let (_, pos_id) = convert_with_identity::<f64>(&mut pos_zero).unwrap();
        let (_, neg_id) = convert_with_identity::<f64>(&mut neg_zero).unwrap();
        assert_ne!(pos_id, neg_id);
    }

    #[test]
    fn i32_semantic_identity_canonicalizes_integer_and_number() {
        let mut int_raw = XLOPER12::integer(42);
        let mut num_raw = XLOPER12::number(42.0);
        let (int_val, int_id) = convert_with_identity::<i32>(&mut int_raw).unwrap();
        let (num_val, num_id) = convert_with_identity::<i32>(&mut num_raw).unwrap();
        assert_eq!(int_val, 42);
        assert_eq!(num_val, 42);
        assert_eq!(int_id, num_id);
    }

    #[test]
    fn vec_semantic_identity_ignores_1d_orientation() {
        let mut row_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut col_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut row_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: row_elements.as_mut_ptr(),
                    rows: 1,
                    columns: 3,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let mut col_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: col_elements.as_mut_ptr(),
                    rows: 3,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let (row_vec, row_id) = convert_with_identity::<Vec<f64>>(&mut row_raw).unwrap();
        let (col_vec, col_id) = convert_with_identity::<Vec<f64>>(&mut col_raw).unwrap();
        assert_eq!(row_vec, vec![1.0, 2.0, 3.0]);
        assert_eq!(col_vec, vec![1.0, 2.0, 3.0]);
        assert_eq!(row_id, col_id);
    }

    #[test]
    fn matrix_semantic_identity_observes_orientation() {
        let mut row_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut col_elements = vec![
            XLOPER12::number(1.0),
            XLOPER12::number(2.0),
            XLOPER12::number(3.0),
        ];
        let mut row_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: row_elements.as_mut_ptr(),
                    rows: 1,
                    columns: 3,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let mut col_raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: col_elements.as_mut_ptr(),
                    rows: 3,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let (row_mat, row_id) = convert_with_identity::<Matrix<f64>>(&mut row_raw).unwrap();
        let (col_mat, col_id) = convert_with_identity::<Matrix<f64>>(&mut col_raw).unwrap();
        assert_eq!((row_mat.rows(), row_mat.columns()), (1, 3));
        assert_eq!((col_mat.rows(), col_mat.columns()), (3, 1));
        assert_ne!(row_id, col_id);
    }

    #[test]
    fn excel_cell_value_canonicalizes_numbers_into_same_identity() {
        let mut int_raw = XLOPER12::integer(10);
        let mut num_raw = XLOPER12::number(10.0);
        let (int_cell, int_id) = convert_with_identity::<ExcelCellValue>(&mut int_raw).unwrap();
        let (num_cell, num_id) = convert_with_identity::<ExcelCellValue>(&mut num_raw).unwrap();
        assert_eq!(int_cell, ExcelCellValue::Number(10.0));
        assert_eq!(num_cell, ExcelCellValue::Number(10.0));
        assert_eq!(int_id, num_id);
    }

    #[test]
    fn excel_value_semantic_identity_canonicalizes_scalars_and_preserves_array_shape() {
        let mut int_raw = XLOPER12::integer(10);
        let mut num_raw = XLOPER12::number(10.0);
        let (int_val, int_id) = convert_with_identity::<ExcelValue>(&mut int_raw).unwrap();
        let (num_val, num_id) = convert_with_identity::<ExcelValue>(&mut num_raw).unwrap();
        assert_eq!(int_val, ExcelValue::Scalar(ExcelCellValue::Number(10.0)));
        assert_eq!(num_val, ExcelValue::Scalar(ExcelCellValue::Number(10.0)));
        assert_eq!(int_id, num_id);
    }

    #[test]
    fn option_and_optional_excel_value_missing_and_blank_identities() {
        let mut missing_raw = XLOPER12::missing();
        let mut blank_raw = XLOPER12::nil();
        let (opt_m, id_m) = convert_with_identity::<Option<f64>>(&mut missing_raw).unwrap();
        let (opt_b, id_b) = convert_with_identity::<Option<f64>>(&mut blank_raw).unwrap();
        assert_eq!(opt_m, None);
        assert_eq!(opt_b, None);
        assert_eq!(id_m, id_b);

        let mut missing_raw2 = XLOPER12::missing();
        let mut blank_raw2 = XLOPER12::nil();
        let (opt_val_m, id_val_m) =
            convert_with_identity::<OptionalExcelValue<f64>>(&mut missing_raw2).unwrap();
        let (opt_val_b, id_val_b) =
            convert_with_identity::<OptionalExcelValue<f64>>(&mut blank_raw2).unwrap();
        assert_eq!(opt_val_m, OptionalExcelValue::Missing);
        assert_eq!(opt_val_b, OptionalExcelValue::Blank);
        assert_ne!(id_val_m, id_val_b);
    }

    #[cfg(feature = "handles")]
    #[derive(Debug, PartialEq)]
    struct SemanticHandleTestObj {
        data: i32,
    }
    #[cfg(feature = "handles")]
    impl crate::handle::ExcelHandleObject for SemanticHandleTestObj {}

    #[cfg(feature = "handles")]
    #[test]
    fn handle_semantic_identity_matches_across_distinct_alias_tokens() {
        use crate::handle::{FormulaCaller, FormulaRevisionKey, HandleTopicKey};

        let slot: &'static crate::handle::FormulaHandleServiceSlot =
            Box::leak(Box::new(crate::handle::FormulaHandleServiceSlot::new()));
        slot.arm(crate::RuntimeConfig::new().handle_config())
            .unwrap();
        slot.initialize().unwrap();
        let handle_rt = slot.read().unwrap();

        let topic_a = HandleTopicKey::Formula(FormulaRevisionKey::new(
            FormulaCaller {
                sheet_id: 1,
                row: 1,
                column: 1,
            },
            "FUNC.A",
            crate::input_identity::InputFingerprint::from_bytes([1; 32]),
        ));
        let topic_b = HandleTopicKey::Formula(FormulaRevisionKey::new(
            FormulaCaller {
                sheet_id: 1,
                row: 2,
                column: 2,
            },
            "FUNC.B",
            crate::input_identity::InputFingerprint::from_bytes([2; 32]),
        ));

        let token_a = handle_rt
            .prepare::<SemanticHandleTestObj, _>(topic_a, || Ok(SemanticHandleTestObj { data: 99 }))
            .unwrap()
            .into_token();

        let token_b = crate::value::with_excel_call_scope(|scope| {
            let resolved: crate::handle::Handle<'_, SemanticHandleTestObj> =
                handle_rt.lookup(scope, &token_a).unwrap();
            handle_rt
                .prepare_observed_alias::<SemanticHandleTestObj, _>(
                    topic_b,
                    resolved.alias(),
                    |_, _| Ok(()),
                )
                .unwrap()
                .into_token()
        });

        assert_ne!(token_a, token_b);

        let mut str_bytes_a: Vec<u16> = std::iter::once(token_a.len() as u16)
            .chain(token_a.encode_utf16())
            .collect();
        let mut raw_a = XLOPER12 {
            value: XLOPER12Value {
                string: str_bytes_a.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };

        let mut str_bytes_b: Vec<u16> = std::iter::once(token_b.len() as u16)
            .chain(token_b.encode_utf16())
            .collect();
        let mut raw_b = XLOPER12 {
            value: XLOPER12Value {
                string: str_bytes_b.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };

        let (handle_data_a, id_a, object_id_a) = crate::call::with_excel_call_scope(|scope| {
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_handle_access(
                scope,
                crate::handle::FormulaHandleServiceResolver::new(slot),
                1,
            );
            // SAFETY: raw_a is live for this conversion.
            let handle = unsafe {
                argument_from_raw_with_arguments::<
                    FormulaInputMode,
                    crate::handle::Handle<'_, SemanticHandleTestObj>,
                >(&mut arguments, 0, "arg", &mut raw_a)
            }
            .unwrap();
            let id = arguments.finish().unwrap().unwrap();
            (handle.data, id, handle.object_id())
        });

        let (handle_data_b, id_b, object_id_b) = crate::call::with_excel_call_scope(|scope| {
            let mut arguments = ArgumentContext::<FormulaInputMode>::from_handle_access(
                scope,
                crate::handle::FormulaHandleServiceResolver::new(slot),
                1,
            );
            // SAFETY: raw_b is live for this conversion.
            let handle = unsafe {
                argument_from_raw_with_arguments::<
                    FormulaInputMode,
                    crate::handle::Handle<'_, SemanticHandleTestObj>,
                >(&mut arguments, 0, "arg", &mut raw_b)
            }
            .unwrap();
            let id = arguments.finish().unwrap().unwrap();
            (handle.data, id, handle.object_id())
        });

        assert_eq!(handle_data_a, 99);
        assert_eq!(handle_data_b, 99);
        assert_eq!(object_id_a, object_id_b);
        assert_eq!(id_a, id_b);
    }
}
