//! Strict worksheet input conversion and explicit scalar and collection values.
//!
//! Input values preserve blank and omitted arguments. Worksheet outputs use
//! [`ExcelCellOutput`](crate::value::ExcelCellOutput) and explicit
//! [`Row`](crate::value::Row), [`Column`](crate::value::Column), or
//! [`Matrix`](crate::value::Matrix) shapes;
//! they cannot preserve the input-only absence distinctions.

use crate::error::{DomainErrorCode, InputError};
use crate::{ExcelError, XllError, XllResult};
use xlfn_sys::XLOPER12;
#[cfg(test)]
use xlfn_sys::XLOPER12Array;

/// Composable presence and collection conversions for custom input types.
pub mod convert;
/// Excel serial-date policy and value types.
mod date;
/// Input conversion traits and presence/default handling.
#[cfg(feature = "serde")]
fn serialize_finite_number<S: serde::Serializer>(
    number: &f64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if !number.is_finite() {
        return Err(serde::ser::Error::custom("Excel numbers must be finite"));
    }
    serializer.serialize_f64(*number)
}

#[cfg(feature = "serde")]
fn deserialize_finite_number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<f64, D::Error> {
    let number = <f64 as serde::Deserialize>::deserialize(deserializer)?;
    if !number.is_finite() {
        return Err(serde::de::Error::custom("Excel numbers must be finite"));
    }
    Ok(number)
}

#[allow(
    unsafe_code,
    reason = "Raw XLOPER12 input conversion is isolated in this leaf"
)]
pub(crate) mod input;
/// Owned rectangular and bounded collection values.
mod matrix;
/// Output conversion traits and return-cell representations.
pub(crate) mod output;
#[cfg(feature = "bench-internals")]
pub(crate) mod prepared_probe;
/// Raw, borrowed views over Excel's XLOPER12 input representation.
#[allow(unsafe_code, reason = "Raw XLOPER12 views are the value ABI leaf")]
mod raw;

pub use crate::input_identity::InputIdentityEncoder;
#[cfg(any(test, feature = "handles", feature = "bench-internals"))]
pub(crate) use input::FormulaInputMode;
use input::PreparedArgument;
#[cfg(test)]
pub(crate) use input::argument_from_raw;
#[cfg(all(test, feature = "handles"))]
pub(crate) use input::argument_from_raw_with_context;
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use input::{ArgumentContext, argument_from_raw_with_arguments};
pub(crate) use input::{CallContext, ExcelParameter};
pub(crate) use input::{ExcelInputCells, PreparedExcelSequence};
pub use input::{ExcelInputIdentity, FromExcel, PrepareExcel};
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

/// An owned input cell, preserving a blank separately from an empty string.
///
/// This input representation is not an implicit worksheet output. Construct
/// an [`ExcelCellOutput`] to choose the meaning of a blank explicitly.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExcelCellValue {
    /// A finite Excel number.
    Number(
        #[cfg_attr(
            feature = "serde",
            serde(
                serialize_with = "serialize_finite_number",
                deserialize_with = "deserialize_finite_number"
            )
        )]
        f64,
    ),
    /// An Excel Boolean without coercion.
    Boolean(bool),
    /// An owned UTF-8 string decoded from Excel's UTF-16 representation.
    String(String),
    /// An Excel error value.
    Error(ExcelError),
    /// A blank cell, distinct from an empty string.
    Blank,
}

/// A zero-allocation view of one worksheet cell for a synchronous call.
///
/// The string variant borrows UTF-8 text from the active call scope. This
/// type is therefore suitable for synchronous and main-thread UDFs, but it
/// cannot be moved into an asynchronous future.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExcelCellRef<'call> {
    /// A finite Excel number.
    Number(f64),
    /// An Excel Boolean without coercion.
    Boolean(bool),
    /// UTF-8 text borrowed from the active call scope.
    String(&'call str),
    /// An Excel error value.
    Error(ExcelError),
    /// A blank cell, distinct from an empty string.
    Blank,
}

/// An owned, dynamic input argument that preserves shape and omission.
///
/// The array contains cells, not nested arrays or omitted arguments. Convert
/// this input deliberately into an output representation before returning it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExcelValue {
    /// One scalar cell.
    Scalar(ExcelCellValue),
    /// An omitted worksheet argument.
    Missing,
    /// A row-major rectangular input array.
    Array(Matrix<ExcelCellValue>),
}

/// A single worksheet cell in the final semantic return representation.
///
/// Unlike [`ExcelCellValue`], this type cannot represent an omitted or blank
/// cell. Use an explicit empty string or [`ExcelError::NotAvailable`] when that
/// is the intended worksheet result.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ExcelCellOutput {
    /// A finite worksheet number; conversion rejects non-finite values.
    Number(
        #[cfg_attr(
            feature = "serde",
            serde(
                serialize_with = "serialize_finite_number",
                deserialize_with = "deserialize_finite_number"
            )
        )]
        f64,
    ),
    /// A worksheet Boolean.
    Boolean(bool),
    /// An owned worksheet string, including an explicit empty string.
    String(String),
    /// An intentional worksheet error value.
    Error(ExcelError),
}

/// An input-only distinction between an omitted and a blank Excel argument.
///
/// Excel does not preserve these meanings for UDF return values: both are
/// displayed as numeric zero. Return an explicit value, empty string, or
/// [`ExcelError`] instead.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OptionalExcelValue<T> {
    /// The argument was omitted from the formula.
    Missing,
    /// The supplied cell was blank.
    Blank,
    /// A present argument converted to `T`.
    Value(T),
}

#[allow(
    unsafe_code,
    reason = "XLOPER12 numeric union projection is audited here"
)]
impl<'call> FromExcel<'call> for f64 {
    #[inline]
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
        crate::utf16::decode_owned(value.utf16(argument)?, argument)
    }
}

impl ExcelInputIdentity for String {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.string(self);
    }
}

impl<'call> PrepareExcel<'call> for XlArrayRef<'call> {
    type Prepared = Self;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
}
impl<'call> PrepareExcel<'call> for f64 {
    type Prepared = Self;
    const __BORROWED_ELEMENTS: bool = true;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.borrowed_f64(identity)
    }
}
impl<'call> PrepareExcel<'call> for bool {
    type Prepared = Self;
    const __BORROWED_ELEMENTS: bool = true;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.borrowed::<Self>(identity)
    }
}
impl<'call> PrepareExcel<'call> for i32 {
    type Prepared = Self;
    const __BORROWED_ELEMENTS: bool = true;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.borrowed::<Self>(identity)
    }
}
impl<'call> PrepareExcel<'call> for i64 {
    type Prepared = Self;
    const __BORROWED_ELEMENTS: bool = true;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.borrowed::<Self>(identity)
    }
}
impl<'call> PrepareExcel<'call> for ExcelError {
    type Prepared = Self;
    const __BORROWED_ELEMENTS: bool = true;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.borrowed::<Self>(identity)
    }
}
impl<'call> PrepareExcel<'call> for ExcelSerialDate {
    type Prepared = Self;
    const __BORROWED_ELEMENTS: bool = true;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.borrowed::<Self>(identity)
    }
}
impl<'call> PrepareExcel<'call> for ExcelCellValue {
    type Prepared = Self;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
}
impl<'call> PrepareExcel<'call> for ExcelValue {
    type Prepared = Self;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        Self::from_excel_with_identity(value, argument, identity)
    }
    fn materialize(value: Self) -> XllResult<Self> {
        Ok(value)
    }
}
/// Validated borrowed UTF-16 input; construction remains inside preparation.
pub struct PreparedString<'call> {
    units: &'call [u16],
    argument: &'static str,
}
impl<'call> PrepareExcel<'call> for String {
    type Prepared = PreparedString<'call>;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self::Prepared> {
        let units = value.utf16(argument)?;
        identity.semantic_utf16(units)?;
        Ok(PreparedString { units, argument })
    }
    fn materialize(value: Self::Prepared) -> XllResult<Self> {
        crate::utf16::decode_owned(value.units, value.argument)
    }
    fn __prepare_elements(
        cells: ExcelInputCells<'call>,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<PreparedExcelSequence<'call, Self, Self::Prepared>> {
        cells.borrowed::<Self>(identity)
    }
}

impl<'call, M: InputMode> input::sealed::ExcelParameterSealed<'call, M> for &'call str {}

impl<'call, M: InputMode> ExcelParameter<'call, M> for &'call str {
    type Prepared = ();

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
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
impl<'call> FromExcel<'call> for ExcelError {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        if value.value_type() != XlValueType::Error {
            return Err(value.wrong_type(argument, "Excel error"));
        }
        // SAFETY: XLTYPE_ERR selects the error member.
        let code = unsafe { value.raw.value.error };
        ExcelError::from_code(code)
            .ok_or_else(|| XllError::input(argument, InputError::Malformed("unknown error code")))
    }
}

impl ExcelInputIdentity for ExcelError {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.i64(i64::from(self.code()));
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

/// Accounts for both the host array and converted Rust data before allocation.
/// All collection conversion paths share this policy, including call-local
/// borrowed slices and owned callback values.
struct ArrayInputBudget {
    argument: &'static str,
    referenced_bytes: usize,
}

impl ArrayInputBudget {
    fn new<T>(element_count: usize, argument: &'static str) -> XllResult<Self> {
        let output_bytes = element_count
            .checked_mul(std::mem::size_of::<T>())
            .ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("output byte-size overflow"))
            })?;
        let referenced_bytes = element_count
            .checked_mul(std::mem::size_of::<XLOPER12>())
            .and_then(|bytes| bytes.checked_add(output_bytes))
            .ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array byte-size overflow"))
            })?;
        let budget = Self {
            argument,
            referenced_bytes,
        };
        budget.check_limit()?;
        Ok(budget)
    }

    #[inline]
    fn include(&mut self, element: XlValueRef<'_>) -> XllResult<()> {
        if element.value_type() == XlValueType::Multi {
            return Err(XllError::input(
                self.argument,
                InputError::Malformed("nested arrays are not supported"),
            ));
        }
        if element.value_type() == XlValueType::String {
            // UTF-16 source storage plus the maximum UTF-8 bytes per unit.
            let string_bytes = element
                .utf16(self.argument)?
                .len()
                .checked_mul(std::mem::size_of::<u16>() + 3)
                .ok_or_else(|| {
                    XllError::input(
                        self.argument,
                        InputError::Malformed("array string byte-size overflow"),
                    )
                })?;
            self.referenced_bytes =
                self.referenced_bytes
                    .checked_add(string_bytes)
                    .ok_or_else(|| {
                        XllError::input(
                            self.argument,
                            InputError::Malformed("array byte-size overflow"),
                        )
                    })?;
            self.check_limit()?;
        }
        Ok(())
    }

    fn check_limit(&self) -> XllResult<()> {
        if self.referenced_bytes > MAX_ARRAY_BYTES {
            return Err(XllError::input(
                self.argument,
                InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    actual: self.referenced_bytes,
                },
            ));
        }
        Ok(())
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
    convert_owned_grid_elements(grid, argument, |element| {
        T::decode(element, argument, context, identity)
    })
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
    let cells = grid.cells();
    let mut budget = ArrayInputBudget::new::<T>(cells.len(), argument)?;
    context.scratch().collect_copy(cells.len(), |index| {
        let element = XlValueRef::from_array_cell(&cells[index])?;
        budget.include(element)?;
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
    type Prepared = OptionalExcelValue<PreparedArgument<T, T::Prepared>>;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        M::tag(
            identity,
            match value.value_type() {
                XlValueType::Missing => 0,
                XlValueType::Nil => 1,
                _ => 2,
            },
        );
        let prepared = convert::optional_value(value, argument, |value, argument| {
            T::prepare(value, argument, context, identity)
        })?;
        Ok(PreparedArgument::Prepared {
            value: prepared,
            materialize: |value| match value {
                OptionalExcelValue::Missing => Ok(Self::Missing),
                OptionalExcelValue::Blank => Ok(Self::Blank),
                OptionalExcelValue::Value(value) => value.materialize().map(Self::Value),
            },
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        M::tag(
            identity,
            match value.value_type() {
                XlValueType::Missing => 0,
                XlValueType::Nil => 1,
                _ => 2,
            },
        );
        convert::optional_value(value, argument, |value, argument| {
            T::decode(value, argument, context, identity)
        })
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
    type Prepared = Option<PreparedArgument<T, T::Prepared>>;
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        M::bool(
            identity,
            !matches!(value.value_type(), XlValueType::Missing | XlValueType::Nil),
        );
        let prepared = convert::optional(value, argument, |value, argument| {
            T::prepare(value, argument, context, identity)
        })?;
        Ok(PreparedArgument::Prepared {
            value: prepared,
            materialize: |value| value.map(PreparedArgument::materialize).transpose(),
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        M::bool(
            identity,
            !matches!(value.value_type(), XlValueType::Missing | XlValueType::Nil),
        );
        convert::optional(value, argument, |value, argument| {
            T::decode(value, argument, context, identity)
        })
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
    type Prepared = (usize, usize, &'static str, T::Elements);
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, rows as u64);
        M::u64(identity, columns as u64);
        let elements = T::prepare_elements(ExcelInputCells { grid, argument }, context, identity)?;
        Ok(PreparedArgument::Prepared {
            value: (rows, columns, argument, elements),
            materialize: |(rows, columns, argument, elements)| {
                let _ = (rows, columns, argument);
                let values = T::materialize_elements(elements)?;
                Matrix::new(rows, columns, values)
            },
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
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
    type Prepared = (
        usize,
        usize,
        T::Elements,
        &'call crate::call::CallScope<'call>,
    );
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY || !T::DEFER_BORROWED_MATRIX {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, rows as u64);
        M::u64(identity, columns as u64);
        let elements = T::prepare_elements(ExcelInputCells { grid, argument }, context, identity)?;
        Ok(PreparedArgument::Prepared {
            value: (rows, columns, elements, context.scope()),
            materialize: |(rows, columns, elements, scope)| {
                let data = T::materialize_elements_borrowed(elements, scope)?;
                MatrixRef::from_slice(rows, columns, data)
            },
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
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
    type Prepared = (usize, usize, &'static str, T::Elements);
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        let grid = convert::grid(value, argument, convert::GridShape::Vector)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, grid.cells().len() as u64);
        let elements = T::prepare_elements(ExcelInputCells { grid, argument }, context, identity)?;
        Ok(PreparedArgument::Prepared {
            value: (rows, columns, argument, elements),
            materialize: |(rows, columns, argument, elements)| {
                let _ = (rows, columns, argument);
                let values = T::materialize_elements(elements)?;
                Ok(values)
            },
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = convert::grid(value, argument, convert::GridShape::Vector)?;
        M::u64(identity, grid.cells().len() as u64);
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
    type Prepared = (usize, usize, &'static str, T::Elements);
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        let grid = convert::bounded_grid::<MAX>(value, argument)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, grid.cells().len() as u64);
        let elements = T::prepare_elements(ExcelInputCells { grid, argument }, context, identity)?;
        Ok(PreparedArgument::Prepared {
            value: (rows, columns, argument, elements),
            materialize: |(rows, columns, argument, elements)| {
                let _ = (rows, columns, argument);
                let values = T::materialize_elements(elements)?;
                Self::new(values).map_err(|error| match error {
                    XllError::Input { reason, .. } => XllError::Input { argument, reason },
                    other => other,
                })
            },
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = convert::bounded_grid::<MAX>(value, argument)?;
        let actual = grid.cells().len();
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
    type Prepared = (usize, usize, &'static str, T::Elements);
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        let grid = convert::grid(value, argument, convert::GridShape::Row)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, grid.cells().len() as u64);
        let elements = T::prepare_elements(ExcelInputCells { grid, argument }, context, identity)?;
        Ok(PreparedArgument::Prepared {
            value: (rows, columns, argument, elements),
            materialize: |(rows, columns, argument, elements)| {
                let _ = (rows, columns, argument);
                let values = T::materialize_elements(elements)?;
                Ok(Self(values))
            },
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = convert::grid(value, argument, convert::GridShape::Row)?;
        M::u64(identity, grid.cells().len() as u64);
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
    type Prepared = (usize, usize, &'static str, T::Elements);
    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedArgument<Self, Self::Prepared>> {
        if !M::RECORDS_IDENTITY {
            return Self::decode(value, argument, context, identity).map(PreparedArgument::Ready);
        }
        let grid = convert::grid(value, argument, convert::GridShape::Column)?;
        let (rows, columns) = grid.shape();
        M::u64(identity, grid.cells().len() as u64);
        let elements = T::prepare_elements(ExcelInputCells { grid, argument }, context, identity)?;
        Ok(PreparedArgument::Prepared {
            value: (rows, columns, argument, elements),
            materialize: |(rows, columns, argument, elements)| {
                let _ = (rows, columns, argument);
                let values = T::materialize_elements(elements)?;
                Ok(Self(values))
            },
        })
    }

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let grid = convert::grid(value, argument, convert::GridShape::Column)?;
        M::u64(identity, grid.cells().len() as u64);
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
            XlValueType::Error => ExcelError::from_excel(value, argument).map(Self::Error),
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
    type Prepared = ();

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let decoded = match value.value_type() {
            XlValueType::Number | XlValueType::Integer => {
                let number = <f64 as FromExcel>::from_excel(value, argument)?;
                Self::Number(number)
            }
            XlValueType::Boolean => {
                let boolean = <bool as FromExcel>::from_excel(value, argument)?;
                Self::Boolean(boolean)
            }
            XlValueType::String => {
                let text = context
                    .scratch()
                    .decode_utf16(value.utf16(argument)?, argument)?;
                Self::String(text)
            }
            XlValueType::Error => {
                let error = <ExcelError as FromExcel>::from_excel(value, argument)?;
                Self::Error(error)
            }
            XlValueType::Nil => Self::Blank,
            _ => return Err(value.wrong_type(argument, "worksheet cell")),
        };
        <Self as ExcelParameter<'call, M>>::encode_decoded(&decoded, identity);
        Ok(decoded)
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

fn convert_owned_grid_elements<'call, T>(
    grid: &GridView<'call>,
    argument: &'static str,
    mut convert: impl FnMut(XlValueRef<'call>) -> XllResult<T>,
) -> XllResult<Vec<T>> {
    let cells = grid.cells();
    let mut budget = ArrayInputBudget::new::<T>(cells.len(), argument)?;
    let mut data = Vec::with_capacity(cells.len());
    for element in cells.iter().map(XlValueRef::from_array_cell) {
        let element = element?;
        budget.include(element)?;
        data.push(convert(element)?);
    }
    Ok(data)
}

impl<'call> FromExcel<'call> for ExcelValue {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        match value.value_type() {
            XlValueType::Missing => Ok(Self::Missing),
            XlValueType::Multi => {
                convert::matrix(value, argument, ExcelCellValue::from_excel).map(Self::Array)
            }
            _ => ExcelCellValue::from_excel(value, argument).map(Self::Scalar),
        }
    }

    fn from_excel_with_identity(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self> {
        if value.value_type() != XlValueType::Multi {
            let converted = Self::from_excel(value, argument)?;
            converted.encode_input_identity(identity);
            return Ok(converted);
        }
        let grid = GridView::from_value(value, argument)?;
        let (rows, columns) = grid.shape();
        identity.tag(3);
        identity.u64(rows as u64);
        identity.u64(columns as u64);
        let cells = convert_owned_grid_elements(&grid, argument, |cell| {
            ExcelCellValue::from_excel_with_identity(cell, argument, identity)
        })?;
        Matrix::new(rows, columns, cells).map(Self::Array)
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
    type Prepared = ();

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }
    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self> {
        let handle =
            crate::handle::with_utf16_handle_token(value.utf16(argument)?, argument, |token| {
                context.resolve_handle::<T>(token)
            })?;
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
        if let Self::Number(value) = &self {
            output::validate_number(*value)?;
        }
        Ok(self)
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_cell(self)
    }
}

impl IntoExcel for f64 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        output::validate_number(self).map(ExcelCellOutput::Number)
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

macro_rules! into_excel_via_f64 {
    ($($type:ty),+ $(,)?) => {
        $(impl IntoExcel for $type {
            fn into_excel(self) -> XllResult<ExcelCellOutput> {
                IntoExcel::into_excel(f64::from(self))
            }

            fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
                sink.push_f64(f64::from(self))
            }
        })+
    };
}

// Each conversion is exact in binary64. f32 retains the finite-number policy.
into_excel_via_f64!(f32, i8, i16, u8, u16, u32);

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

impl IntoExcel for u64 {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        if self <= 1_u64 << 53 {
            Ok(ExcelCellOutput::Number(self as f64))
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
        }
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        if self <= 1_u64 << 53 {
            sink.push_f64(self as f64)
        } else {
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
        }
    }
}

impl IntoExcel for usize {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        IntoExcel::into_excel(self as u64)
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        (self as u64).write_into(sink)
    }
}

impl IntoExcel for isize {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        IntoExcel::into_excel(self as i64)
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        (self as i64).write_into(sink)
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
        sink.push_str(self)
    }
}

impl IntoExcel for ExcelError {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        Ok(ExcelCellOutput::Error(self))
    }

    fn write_into<S: output::ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
        sink.push_error(self)
    }
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "Value tests exercise the audited raw input boundary"
)]
mod tests {
    use super::*;
    use crate::call::with_excel_call_scope;
    use crate::call_return::{
        AsyncReturn, ExcelReturn, MacroSheetReturn, MainThreadReturn, ReturnContext, ReturnPayload,
        ThreadSafeReturn, VolatileReturn,
    };
    use crate::return_abi::XlArrayBuilder;
    use proptest::prelude::*;
    use static_assertions::assert_impl_all;
    use xlfn_sys::{XLBIT_XL_FREE, XLOPER12Value, XLTYPE_MULTI, XLTYPE_STR};

    assert_impl_all!(ExcelValue: std::panic::UnwindSafe, std::panic::RefUnwindSafe);
    assert_impl_all!(ExcelError: MainThreadReturn, ThreadSafeReturn, MacroSheetReturn, AsyncReturn, VolatileReturn);
    assert_impl_all!(f32: MainThreadReturn, ThreadSafeReturn, AsyncReturn);
    assert_impl_all!(u32: MainThreadReturn, ThreadSafeReturn, AsyncReturn);
    assert_impl_all!(usize: MainThreadReturn, ThreadSafeReturn, AsyncReturn);
    assert_impl_all!(
        XlArrayBuilder: std::panic::UnwindSafe, std::panic::RefUnwindSafe
    );

    #[test]
    fn excel_error_is_preserved_without_a_value_wrapper() {
        for error in [
            ExcelError::Null,
            ExcelError::DivisionByZero,
            ExcelError::Value,
            ExcelError::Reference,
            ExcelError::Name,
            ExcelError::Number,
            ExcelError::NotAvailable,
            ExcelError::GettingData,
        ] {
            let mut raw = XLOPER12::error(error.code());
            assert_eq!(convert::<ExcelError>(&mut raw).unwrap(), error);
            assert_eq!(
                IntoExcel::into_excel(error).unwrap(),
                ExcelCellOutput::Error(error)
            );
        }
    }

    #[test]
    fn unsigned_and_platform_integer_outputs_reject_precision_loss() {
        let exact_limit = 1_u64 << 53;
        for value in [0, u64::from(u32::MAX), exact_limit - 1, exact_limit] {
            assert_eq!(
                IntoExcel::into_excel(value).unwrap(),
                ExcelCellOutput::Number(value as f64)
            );
            let mut builder = XlArrayBuilder::new(1, 1).unwrap();
            value.write_into(&mut builder).unwrap();
            builder.finish().unwrap();
        }
        for value in [exact_limit + 1, u64::MAX] {
            assert!(matches!(
                IntoExcel::into_excel(value),
                Err(XllError::Domain {
                    code: DomainErrorCode::Overflow
                })
            ));
            let mut builder = XlArrayBuilder::new(1, 1).unwrap();
            assert!(matches!(
                value.write_into(&mut builder),
                Err(XllError::Domain {
                    code: DomainErrorCode::Overflow
                })
            ));
        }
        assert_eq!(
            IntoExcel::into_excel(u32::MAX).unwrap(),
            ExcelCellOutput::Number(u32::MAX as f64)
        );
        assert_eq!(
            IntoExcel::into_excel(42_usize).unwrap(),
            ExcelCellOutput::Number(42.0)
        );
        assert_eq!(
            IntoExcel::into_excel(-42_isize).unwrap(),
            ExcelCellOutput::Number(-42.0)
        );
        #[cfg(target_pointer_width = "64")]
        {
            assert!(IntoExcel::into_excel(usize::MAX).is_err());
            assert!(IntoExcel::into_excel(isize::MAX).is_err());
            assert!(IntoExcel::into_excel(isize::MIN).is_err());
        }
    }

    #[test]
    fn f32_outputs_preserve_values_and_reject_non_finite_numbers() {
        for value in [-0.0_f32, f32::MIN_POSITIVE, f32::MAX] {
            let ExcelCellOutput::Number(actual) = IntoExcel::into_excel(value).unwrap() else {
                panic!("f32 did not convert to a number");
            };
            assert_eq!(actual.to_bits(), f64::from(value).to_bits());
        }
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(
                IntoExcel::into_excel(value),
                Err(XllError::Domain {
                    code: DomainErrorCode::InvalidInput
                })
            ));
            let mut builder = XlArrayBuilder::new(1, 1).unwrap();
            assert!(matches!(
                value.write_into(&mut builder),
                Err(XllError::Domain {
                    code: DomainErrorCode::InvalidInput
                })
            ));
        }
    }

    #[derive(Default)]
    struct BorrowTrackingSink {
        borrowed_string_calls: usize,
    }

    impl output::ExcelCellSink for BorrowTrackingSink {
        fn push_cell(&mut self, _: ExcelCellOutput) -> XllResult<()> {
            panic!("borrowed string conversion must not use the semantic fallback")
        }

        fn push_f64(&mut self, _: f64) -> XllResult<()> {
            panic!("unexpected numeric output")
        }

        fn push_bool(&mut self, _: bool) -> XllResult<()> {
            panic!("unexpected boolean output")
        }

        fn push_str(&mut self, value: &str) -> XllResult<()> {
            assert_eq!(value, "borrowed-value");
            self.borrowed_string_calls += 1;
            Ok(())
        }

        fn push_string(&mut self, _: String) -> XllResult<()> {
            panic!("&str direct-write must not allocate an owned String")
        }

        fn push_error(&mut self, _: crate::ExcelError) -> XllResult<()> {
            panic!("unexpected error output")
        }
    }

    #[test]
    fn borrowed_str_direct_write_uses_borrowed_sink_path() {
        let mut sink = BorrowTrackingSink::default();
        "borrowed-value".write_into(&mut sink).unwrap();
        assert_eq!(sink.borrowed_string_calls, 1);
    }

    #[test]
    fn derived_excel_enum_direct_write_uses_borrowed_sink_path() {
        #[derive(Clone, Copy, crate::ExcelEnum)]
        enum Label {
            #[excel_enum(name = "borrowed-value")]
            Renamed,
        }

        let mut sink = BorrowTrackingSink::default();
        Label::Renamed.write_into(&mut sink).unwrap();
        assert_eq!(sink.borrowed_string_calls, 1);
        assert!(matches!(
            IntoExcel::into_excel(Label::Renamed).unwrap(),
            ExcelCellOutput::String(value) if value == "borrowed-value"
        ));
    }

    #[test]
    fn derived_excel_enum_array_output_preserves_variant_text_and_sink_errors() {
        #[derive(Clone, Copy, crate::ExcelEnum)]
        enum Label {
            Ready,
            #[excel_enum(name = "日本語💡")]
            Unicode,
        }

        let mut builder = XlArrayBuilder::new(1, 2).unwrap();
        builder.push(Label::Ready).unwrap();
        builder.push(Label::Unicode).unwrap();
        assert!(Label::Ready.write_into(&mut builder).is_err());
        let output = builder.finish().unwrap();
        for (raw, expected) in output.cells.iter().zip(["Ready", "日本語💡"]) {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            assert_eq!(String::from_excel(value, "enum").unwrap(), expected);
        }
    }

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
                value.encode_input_identity(encoder);
                Ok(())
            })
            .unwrap();
        builder.finish().unwrap()
    }

    #[test]
    fn borrowed_cell_identity_preserves_variants_and_matches_decoded_values() {
        use crate::input_identity::InputFingerprintBuilder;

        fn identity(raw: &mut XLOPER12) -> crate::input_identity::InputFingerprint {
            with_excel_call_scope(|scope| {
                let mut during_decode = InputFingerprintBuilder::new(1);
                // SAFETY: the caller keeps the root and its payload live for this scope.
                let borrowed = unsafe { XlValueRef::from_raw(raw) }.unwrap();
                let decoded = during_decode
                    .with_argument(0, "arg", |encoder| {
                        <ExcelCellRef<'_> as ExcelParameter<FormulaInputMode>>::decode(
                            borrowed,
                            "arg",
                            &CallContext::plain(scope),
                            encoder,
                        )
                    })
                    .unwrap();
                let mut after_decode = InputFingerprintBuilder::new(1);
                after_decode
                    .with_argument(0, "arg", |encoder| {
                        decoded.encode_input_identity(encoder);
                        Ok(())
                    })
                    .unwrap();
                let fingerprint = during_decode.finish().unwrap();
                assert_eq!(fingerprint, after_decode.finish().unwrap());
                fingerprint
            })
        }

        let mut text = vec![0];
        text.extend("日本語💡".encode_utf16());
        text[0] = (text.len() - 1) as u16;
        let mut cases = [
            XLOPER12::number(0.0),
            XLOPER12::error(ExcelError::Null.code()),
            XLOPER12::boolean(false),
            XLOPER12 {
                value: XLOPER12Value {
                    string: text.as_mut_ptr(),
                },
                xltype: xlfn_sys::XLTYPE_STR,
            },
            XLOPER12::nil(),
        ];
        let fingerprints = cases.iter_mut().map(identity).collect::<Vec<_>>();
        for (index, fingerprint) in fingerprints.iter().enumerate() {
            for other in &fingerprints[index + 1..] {
                assert_ne!(fingerprint, other);
            }
        }
    }

    #[test]
    fn fused_excel_value_conversion_matches_independent_conversion_and_identity() {
        let mut text = vec![0];
        text.extend("日本語💡".encode_utf16());
        text[0] = (text.len() - 1) as u16;
        let mut cells = [
            XLOPER12::number(-0.0),
            XLOPER12::integer(42),
            XLOPER12::boolean(true),
            XLOPER12 {
                value: XLOPER12Value {
                    string: text.as_mut_ptr(),
                },
                xltype: XLTYPE_STR,
            },
            XLOPER12::error(ExcelError::Null.code()),
            XLOPER12::nil(),
        ];
        let mut inputs = [
            XLOPER12::missing(),
            XLOPER12::number(42.0),
            XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        rows: 2,
                        columns: 3,
                        values: cells.as_mut_ptr(),
                    },
                },
                xltype: XLTYPE_MULTI,
            },
        ];
        for raw in &mut inputs {
            let expected_value = convert::<ExcelValue>(raw).unwrap();
            let mut reference = crate::input_identity::InputFingerprintBuilder::new(1);
            reference
                .with_argument(0, "arg", |encoder| {
                    expected_value.encode_input_identity(encoder);
                    Ok(())
                })
                .unwrap();
            let (actual_value, actual_identity) = convert_with_identity::<ExcelValue>(raw).unwrap();
            assert_eq!(actual_value, expected_value);
            assert_eq!(actual_identity, reference.finish().unwrap());
        }
    }

    #[test]
    fn raw_string_identity_preserves_cell_boundaries_and_surrogate_units() {
        fn identity(strings: &[&[u16]]) -> crate::input_identity::InputFingerprint {
            let mut payloads = strings
                .iter()
                .map(|units| {
                    std::iter::once(units.len() as u16)
                        .chain(units.iter().copied())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let mut cells = payloads
                .iter_mut()
                .map(|text| XLOPER12 {
                    value: XLOPER12Value {
                        string: text.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                })
                .collect::<Vec<_>>();
            let mut raw = XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        rows: 1,
                        columns: cells.len() as i32,
                        values: cells.as_mut_ptr(),
                    },
                },
                xltype: XLTYPE_MULTI,
            };
            with_excel_call_scope(|scope| {
                // SAFETY: root, cells, and counted strings remain live in this scope.
                let view: XlArrayRef<'_> =
                    unsafe { argument_from_raw(scope, "arg", &mut raw) }.unwrap();
                raw_array_identity(view)
            })
        }
        assert_ne!(
            identity(&[&[0x61, 0x62], &[0x63]]),
            identity(&[&[0x61], &[0x62, 0x63]])
        );
        assert_ne!(identity(&[&[0xd800]]), identity(&[&[0xfffd]]));
        assert_ne!(identity(&[&[]]), identity(&[&[0]]));
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
    fn array_input_budget_checks_host_and_converted_storage_before_allocation() {
        let per_cell = std::mem::size_of::<XLOPER12>() + std::mem::size_of::<f64>();
        let maximum = MAX_ARRAY_BYTES / per_cell;
        assert!(ArrayInputBudget::new::<f64>(maximum, "values").is_ok());
        assert!(matches!(
            ArrayInputBudget::new::<f64>(maximum + 1, "values"),
            Err(XllError::Input {
                argument: "values",
                reason: InputError::TooLarge { limit: MAX_ARRAY_BYTES, actual },
            }) if actual == (maximum + 1) * per_cell
        ));
        assert!(matches!(
            ArrayInputBudget::new::<f64>(usize::MAX, "values"),
            Err(XllError::Input {
                reason: InputError::Malformed("output byte-size overflow"),
                ..
            })
        ));
    }

    #[test]
    fn array_input_budget_counts_source_and_decoded_text() {
        let mut text = [1_u16, u16::from(b'x')];
        let raw = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        let value = XlValueRef::from_array_cell(&raw).unwrap();
        let mut budget = ArrayInputBudget {
            argument: "values",
            referenced_bytes: MAX_ARRAY_BYTES - 5,
        };
        assert!(budget.include(value).is_ok());
        assert_eq!(budget.referenced_bytes, MAX_ARRAY_BYTES);
        assert!(matches!(
            budget.include(value),
            Err(XllError::Input {
                argument: "values",
                reason: InputError::TooLarge { limit: MAX_ARRAY_BYTES, actual },
            }) if actual == MAX_ARRAY_BYTES + 5
        ));
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
        let ReturnPayload::Scalar(value) = value else {
            panic!("scalar return expected");
        };
        assert_eq!(
            XlValueRef::from_array_cell(value.as_raw())
                .unwrap()
                .as_f64()
                .unwrap(),
            4.5
        );
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
                string.try_to_string(),
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
            assert_eq!(
                view.shape(),
                crate::error::Shape {
                    rows: 2,
                    columns: 2
                }
            );
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
    #[cfg(feature = "handles")]
    fn raw_array_type_changes_recreate_handle_results() {
        use crate::handle::{
            FormulaCaller, FormulaHandleService, FormulaRevisionKey, HandleTopicKey,
        };
        use crate::input_identity::{InputFingerprint, InputFingerprintBuilder};

        struct ObservedType {
            _kind: XlValueType,
        }
        impl crate::handle::ExcelHandleObject for ObservedType {}

        fn prepare(cell: XLOPER12) -> (InputFingerprint, XlValueType) {
            let mut cells = [cell];
            let raw = XLOPER12 {
                value: XLOPER12Value {
                    array: XLOPER12Array {
                        rows: 1,
                        columns: 1,
                        values: cells.as_mut_ptr(),
                    },
                },
                xltype: XLTYPE_MULTI,
            };
            let value = XlValueRef::from_array_cell(&raw).unwrap();
            let mut builder = InputFingerprintBuilder::new(1);
            let prepared = builder
                .with_argument(0, "values", |identity| {
                    <XlArrayRef<'_> as PrepareExcel>::prepare(value, "values", identity)
                })
                .unwrap();
            let view = <XlArrayRef<'_> as PrepareExcel>::materialize(prepared).unwrap();
            let kind = view.get(0, 0).unwrap().value_type();
            (builder.finish().unwrap(), kind)
        }

        let integer = prepare(XLOPER12::integer(1));
        let same_integer = prepare(XLOPER12::integer(1));
        let number = prepare(XLOPER12::number(1.0));
        let same_number = prepare(XLOPER12::number(1.0));
        let mut flagged_number = XLOPER12::number(1.0);
        flagged_number.xltype |= xlfn_sys::XLBIT_XL_FREE;
        let flagged_number = prepare(flagged_number);
        assert_eq!(integer.1, XlValueType::Integer);
        assert_eq!(number.1, XlValueType::Number);
        assert_ne!(integer.0, number.0);
        assert_eq!(integer.0, same_integer.0);
        assert_eq!(number.0, same_number.0);
        assert_eq!(number, flagged_number);

        let handles = FormulaHandleService::try_new(8).unwrap();
        let mut factory_calls = 0;
        let mut invoke = |input: (InputFingerprint, XlValueType)| {
            let key = HandleTopicKey::Formula(FormulaRevisionKey::new(
                FormulaCaller {
                    sheet_id: 1,
                    row: 1,
                    column: 1,
                },
                "TEST.RAW_ARRAY.TYPE",
                input.0,
            ));
            handles
                .prepare_observed(
                    key,
                    || {
                        factory_calls += 1;
                        Ok(ObservedType { _kind: input.1 })
                    },
                    |_, _| Ok(()),
                )
                .unwrap()
                .into_token()
        };
        let integer_token = invoke(integer);
        assert_eq!(integer_token, invoke(same_integer));
        let number_token = invoke(number);
        assert_ne!(integer_token, number_token);
        assert_eq!(number_token, invoke(same_number));
        assert_eq!(number_token, invoke(flagged_number));
        assert_eq!(factory_calls, 2);
        handles.terminate_all_topics();
        let _ = handles.seal();
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

        let token_b =
            crate::call::with_excel_call_scope_and_state(&handle_rt, |handle_rt, scope| {
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
