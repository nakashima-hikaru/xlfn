//! Raw and borrowed ABI views over Excel's `XLOPER12` representation.

use super::{FromExcel, MAX_ARRAY_BYTES, MAX_ARRAY_ELEMENTS};
use crate::error::InputError;
use crate::input_identity::InputIdentityEncoder;
use crate::{ExcelError, XllError, XllResult};
use std::marker::PhantomData;
use std::rc::Rc;
use std::slice;
use xlfn_sys::{XLBIT_DLL_FREE, XLBIT_XL_FREE, XLOPER12, XLOPER12Array, XLTYPE_MASK};

const EXCEL_MAX_ROWS: usize = 1_048_576;
const EXCEL_MAX_COLUMNS: usize = 16_384;

/// The validated semantic value type of an Excel `XLOPER12`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum XlValueType {
    Number,
    String,
    Boolean,
    Reference,
    Error,
    Flow,
    Multi,
    Missing,
    Nil,
    SimpleReference,
    Integer,
    BigData,
}

impl XlValueType {
    /// Decodes a raw base xltype mask into a validated [`XlValueType`].
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            xlfn_sys::XLTYPE_NUM => Some(Self::Number),
            xlfn_sys::XLTYPE_STR => Some(Self::String),
            xlfn_sys::XLTYPE_BOOL => Some(Self::Boolean),
            xlfn_sys::XLTYPE_REF => Some(Self::Reference),
            xlfn_sys::XLTYPE_ERR => Some(Self::Error),
            xlfn_sys::XLTYPE_FLOW => Some(Self::Flow),
            xlfn_sys::XLTYPE_MULTI => Some(Self::Multi),
            xlfn_sys::XLTYPE_MISSING => Some(Self::Missing),
            xlfn_sys::XLTYPE_NIL => Some(Self::Nil),
            xlfn_sys::XLTYPE_SREF => Some(Self::SimpleReference),
            xlfn_sys::XLTYPE_INT => Some(Self::Integer),
            xlfn_sys::XLTYPE_BIG_DATA => Some(Self::BigData),
            _ => None,
        }
    }

    /// Returns the underlying raw Excel `xltype` constant for this value type.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Number => xlfn_sys::XLTYPE_NUM,
            Self::String => xlfn_sys::XLTYPE_STR,
            Self::Boolean => xlfn_sys::XLTYPE_BOOL,
            Self::Reference => xlfn_sys::XLTYPE_REF,
            Self::Error => xlfn_sys::XLTYPE_ERR,
            Self::Flow => xlfn_sys::XLTYPE_FLOW,
            Self::Multi => xlfn_sys::XLTYPE_MULTI,
            Self::Missing => xlfn_sys::XLTYPE_MISSING,
            Self::Nil => xlfn_sys::XLTYPE_NIL,
            Self::SimpleReference => xlfn_sys::XLTYPE_SREF,
            Self::Integer => xlfn_sys::XLTYPE_INT,
            Self::BigData => xlfn_sys::XLTYPE_BIG_DATA,
        }
    }
}

impl std::fmt::Display for XlValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number => write!(f, "Number"),
            Self::String => write!(f, "String"),
            Self::Boolean => write!(f, "Boolean"),
            Self::Reference => write!(f, "Reference"),
            Self::Error => write!(f, "Error"),
            Self::Flow => write!(f, "Flow"),
            Self::Multi => write!(f, "Multi"),
            Self::Missing => write!(f, "Missing"),
            Self::Nil => write!(f, "Nil"),
            Self::SimpleReference => write!(f, "SimpleReference"),
            Self::Integer => write!(f, "Integer"),
            Self::BigData => write!(f, "BigData"),
        }
    }
}

#[derive(Clone, Copy)]
pub struct XlValueRef<'call> {
    pub(super) raw: &'call XLOPER12,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

pub(crate) enum GridView<'call> {
    Scalar(XlValueRef<'call>),
    Multi {
        rows: usize,
        columns: usize,
        values: &'call [XLOPER12],
    },
}

impl<'call> GridView<'call> {
    pub(crate) fn from_value(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        if value.value_type() == XlValueType::Multi {
            let array = value.array(argument)?;
            let len = (array.rows as usize) * (array.columns as usize);
            let values = if len == 0 {
                &[]
            } else {
                // SAFETY: `XlValueRef::array` validated the non-null pointer,
                // alignment, byte size, and contiguous element range.
                unsafe { slice::from_raw_parts(array.values.cast_const(), len) }
            };
            Ok(Self::Multi {
                rows: array.rows as usize,
                columns: array.columns as usize,
                values,
            })
        } else {
            Ok(Self::Scalar(value))
        }
    }

    pub(crate) const fn shape(&self) -> (usize, usize) {
        match self {
            Self::Scalar(_) => (1, 1),
            Self::Multi { rows, columns, .. } => (*rows, *columns),
        }
    }

    pub(crate) fn cells(&self) -> &'call [XLOPER12] {
        match self {
            Self::Scalar(value) => slice::from_ref(value.raw),
            Self::Multi { values, .. } => values,
        }
    }
}

impl<'call> XlValueRef<'call> {
    /// Creates a call-scoped view over an argument supplied by Excel.
    ///
    /// # Safety
    ///
    /// `raw` must be non-null, aligned, and point to a live XLOPER12 for
    /// `'call`. Any nested pointers selected by `xltype` must satisfy the
    /// corresponding Excel SDK contract.
    pub unsafe fn from_raw(raw: *mut XLOPER12) -> XllResult<Self> {
        // SAFETY: The caller guarantees a live, aligned XLOPER12 for 'call.
        let raw = unsafe { raw.as_ref() }
            .ok_or_else(|| XllError::input("<raw>", InputError::NullPointer))?;
        Self::from_array_cell(raw)
    }

    pub(crate) fn from_array_cell(raw: &'call XLOPER12) -> XllResult<Self> {
        if raw.xltype & !(XLTYPE_MASK | XLBIT_XL_FREE | XLBIT_DLL_FREE) != 0 {
            return Err(XllError::input(
                "<raw>",
                InputError::Malformed("unknown xltype flag"),
            ));
        }
        if XlValueType::from_raw(raw.base_type()).is_none() {
            return Err(XllError::input(
                "<raw>",
                InputError::Malformed("unknown base xltype"),
            ));
        }
        Ok(Self {
            raw,
            _not_send_or_sync: PhantomData,
        })
    }

    #[must_use]
    #[inline]
    pub const fn value_type(self) -> XlValueType {
        match XlValueType::from_raw(self.raw.base_type()) {
            Some(value_type) => value_type,
            None => unreachable!(),
        }
    }

    #[must_use]
    #[inline]
    pub const fn raw(&self) -> &'call XLOPER12 {
        self.raw
    }

    /// Returns the raw `xltype` field value of the underlying `XLOPER12`.
    #[must_use]
    #[inline]
    pub const fn raw_xltype(self) -> u32 {
        self.raw.xltype
    }

    #[inline]
    pub fn as_f64(self) -> XllResult<f64> {
        <f64 as FromExcel>::from_excel(self, "<array cell>")
    }

    #[inline]
    pub fn as_bool(self) -> XllResult<bool> {
        <bool as FromExcel>::from_excel(self, "<array cell>")
    }

    #[inline]
    pub fn as_str(self) -> XllResult<XlStrRef<'call>> {
        self.as_str_with_argument("<array cell>")
    }

    #[inline]
    pub fn as_str_with_argument(self, argument: &'static str) -> XllResult<XlStrRef<'call>> {
        Ok(XlStrRef {
            utf16: self.utf16(argument)?,
            argument,
        })
    }

    #[must_use]
    #[inline]
    pub const fn is_blank(self) -> bool {
        matches!(self.value_type(), XlValueType::Nil)
    }

    pub(crate) fn wrong_type(self, argument: &'static str, expected: &'static str) -> XllError {
        if self.value_type() == XlValueType::Error {
            // SAFETY: XLTYPE_ERR selects the error union member.
            let code = unsafe { self.raw.value.error };
            return ExcelError::from_code(code).map_or_else(
                || XllError::input(argument, InputError::Malformed("unknown error code")),
                XllError::ExcelValue,
            );
        }
        XllError::input(
            argument,
            InputError::WrongType {
                expected,
                actual: self.value_type(),
            },
        )
    }

    pub(crate) fn utf16(&self, argument: &'static str) -> XllResult<&'call [u16]> {
        if self.value_type() != XlValueType::String {
            return Err(self.wrong_type(argument, "string"));
        }
        // SAFETY: XLTYPE_STR selects the string union member.
        let pointer = unsafe { self.raw.value.string };
        if pointer.is_null() {
            return Err(XllError::input(argument, InputError::NullPointer));
        }
        // SAFETY: Excel strings begin with one readable length code unit.
        let length = unsafe { *pointer } as usize;
        if length > crate::utf16::EXCEL_STRING_LIMIT {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: crate::utf16::EXCEL_STRING_LIMIT,
                    actual: length,
                },
            ));
        }
        // SAFETY: pointer points to a valid Excel string with at least length+1 units.
        let data = unsafe { pointer.add(1) };
        // SAFETY: The Excel string contract guarantees length following units.
        Ok(unsafe { slice::from_raw_parts(data, length) })
    }

    pub(crate) fn array(&self, argument: &'static str) -> XllResult<XLOPER12Array> {
        if self.value_type() != XlValueType::Multi {
            return Err(self.wrong_type(argument, "array"));
        }
        // SAFETY: XLTYPE_MULTI selects the array union member.
        let array = unsafe { self.raw.value.array };
        if array.rows < 0 || array.columns < 0 {
            return Err(XllError::input(
                argument,
                InputError::Malformed("negative array dimension"),
            ));
        }
        let rows = array.rows as usize;
        let columns = array.columns as usize;
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
        let elements = rows.checked_mul(columns).ok_or_else(|| {
            XllError::input(argument, InputError::Malformed("array dimension overflow"))
        })?;
        if elements > MAX_ARRAY_ELEMENTS {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: MAX_ARRAY_ELEMENTS,
                    actual: elements,
                },
            ));
        }
        let bytes = elements
            .checked_mul(std::mem::size_of::<XLOPER12>())
            .ok_or_else(|| {
                XllError::input(argument, InputError::Malformed("array byte-size overflow"))
            })?;
        if bytes > MAX_ARRAY_BYTES {
            return Err(XllError::input(
                argument,
                InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    actual: bytes,
                },
            ));
        }
        if elements != 0 && array.values.is_null() {
            return Err(XllError::input(argument, InputError::NullPointer));
        }
        if elements != 0 && !array.values.is_aligned() {
            return Err(XllError::input(
                argument,
                InputError::Malformed("misaligned array pointer"),
            ));
        }
        Ok(array)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XlStrRef<'call> {
    utf16: &'call [u16],
    argument: &'static str,
}

impl<'call> XlStrRef<'call> {
    #[must_use]
    #[inline]
    pub const fn as_utf16(self) -> &'call [u16] {
        self.utf16
    }

    pub fn chars(self) -> impl Iterator<Item = Result<char, std::char::DecodeUtf16Error>> + 'call {
        char::decode_utf16(self.utf16.iter().copied())
    }

    pub fn to_string(self) -> XllResult<String> {
        String::from_utf16(self.utf16)
            .map_err(|_| XllError::input(self.argument, InputError::InvalidUtf16))
    }
}

#[derive(Clone, Copy)]
pub struct XlArrayRef<'call> {
    cells: &'call [XLOPER12],
    rows: usize,
    columns: usize,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<'call> XlArrayRef<'call> {
    fn from_value(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let array = value.array(argument)?;
        let rows = array.rows as usize;
        let columns = array.columns as usize;
        let len = rows * columns;
        let cells = if len == 0 {
            &[]
        } else {
            // SAFETY: XlValueRef::array validated the non-null pointer,
            // dimensions, byte size, and lifetime of this contiguous range.
            unsafe { slice::from_raw_parts(array.values.cast_const(), len) }
        };
        Ok(Self {
            cells,
            rows,
            columns,
            _not_send_or_sync: PhantomData,
        })
    }

    #[must_use]
    pub const fn rows(self) -> usize {
        self.rows
    }

    #[must_use]
    pub const fn columns(self) -> usize {
        self.columns
    }

    #[must_use]
    pub const fn shape(self) -> (usize, usize) {
        (self.rows, self.columns)
    }

    #[must_use]
    pub const fn len(self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.cells.is_empty()
    }

    #[must_use]
    pub fn get(self, row: usize, column: usize) -> Option<XlValueRef<'call>> {
        if row >= self.rows || column >= self.columns {
            return None;
        }
        let index = row * self.columns + column;
        Some(XlValueRef {
            raw: &self.cells[index],
            _not_send_or_sync: PhantomData,
        })
    }

    pub fn cells(self) -> impl ExactSizeIterator<Item = XlValueRef<'call>> + 'call {
        self.cells.iter().map(|raw| XlValueRef {
            raw,
            _not_send_or_sync: PhantomData,
        })
    }
}

impl<'call> FromExcel<'call> for XlArrayRef<'call> {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        Self::from_value(value, argument)
    }
}

#[repr(u8)]
enum RawValueKind {
    Number = 1,
    Boolean = 2,
    String = 3,
    Error = 4,
    Missing = 5,
    Blank = 6,
    Array = 7,
}

pub(crate) fn encode_raw_value(
    value: XlValueRef<'_>,
    nested: bool,
    encoder: &mut InputIdentityEncoder,
) {
    match value.value_type() {
        XlValueType::Number => {
            encoder.tag(RawValueKind::Number as u8);
            // SAFETY: XLTYPE_NUM selects the number union member.
            encoder.u64(unsafe { value.raw.value.number }.to_bits());
        }
        XlValueType::Boolean => {
            encoder.tag(RawValueKind::Boolean as u8);
            // SAFETY: XLTYPE_BOOL selects the boolean union member.
            encoder.u32(unsafe { value.raw.value.boolean } as u32);
        }
        XlValueType::Integer => {
            encoder.tag(RawValueKind::Number as u8);
            // SAFETY: XLTYPE_INT selects the integer union member.
            encoder.f64(unsafe { value.raw.value.integer } as f64);
        }
        XlValueType::String => {
            encoder.tag(RawValueKind::String as u8);
            match value.utf16(encoder.argument()) {
                Ok(text) => {
                    encoder.u64(text.len() as u64);
                    for unit in text {
                        encoder.u32(u32::from(*unit));
                    }
                }
                Err(error) => encoder.fail(error),
            }
        }
        XlValueType::Error => {
            encoder.tag(RawValueKind::Error as u8);
            // SAFETY: XLTYPE_ERR selects the error union member.
            encoder.i64(unsafe { value.raw.value.error } as i64);
        }
        XlValueType::Missing => encoder.tag(RawValueKind::Missing as u8),
        XlValueType::Nil => encoder.tag(RawValueKind::Blank as u8),
        XlValueType::Multi if !nested => match value.array(encoder.argument()) {
            Ok(array) => {
                encoder.tag(RawValueKind::Array as u8);
                encoder.u64(array.rows as u64);
                encoder.u64(array.columns as u64);
                let elements = (array.rows as usize) * (array.columns as usize);
                for index in 0..elements {
                    // SAFETY: XlValueRef::array validated the contiguous
                    // element range and index is within its dimensions.
                    let elem_ptr = unsafe { array.values.add(index) };
                    // SAFETY: elem_ptr is a valid pointer within the array allocation.
                    match unsafe { XlValueRef::from_raw(elem_ptr) } {
                        Ok(element) => encode_raw_value(element, true, encoder),
                        Err(error) => encoder.fail(error),
                    }
                }
            }
            Err(error) => encoder.fail(error),
        },
        XlValueType::Multi => {
            encoder.fail_input(InputError::Malformed("nested arrays are not supported"))
        }
        actual => encoder.fail_input(InputError::WrongType {
            expected: "worksheet value",
            actual,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlfn_sys::{
        XLBIT_DLL_FREE, XLBIT_XL_FREE, XLOPER12Value, XLTYPE_BIG_DATA, XLTYPE_BOOL, XLTYPE_ERR,
        XLTYPE_FLOW, XLTYPE_INT, XLTYPE_MISSING, XLTYPE_MULTI, XLTYPE_NIL, XLTYPE_NUM, XLTYPE_REF,
        XLTYPE_SREF, XLTYPE_STR,
    };

    #[test]
    fn xl_value_type_from_raw_and_raw_round_trip() {
        let cases = [
            (XLTYPE_NUM, XlValueType::Number),
            (XLTYPE_STR, XlValueType::String),
            (XLTYPE_BOOL, XlValueType::Boolean),
            (XLTYPE_REF, XlValueType::Reference),
            (XLTYPE_ERR, XlValueType::Error),
            (XLTYPE_FLOW, XlValueType::Flow),
            (XLTYPE_MULTI, XlValueType::Multi),
            (XLTYPE_MISSING, XlValueType::Missing),
            (XLTYPE_NIL, XlValueType::Nil),
            (XLTYPE_SREF, XlValueType::SimpleReference),
            (XLTYPE_INT, XlValueType::Integer),
            (XLTYPE_BIG_DATA, XlValueType::BigData),
        ];

        for (raw, expected_type) in cases {
            assert_eq!(XlValueType::from_raw(raw), Some(expected_type));
            assert_eq!(expected_type.raw(), raw);
        }

        assert_eq!(XlValueType::from_raw(0), None);
        assert_eq!(XlValueType::from_raw(0x0003), None);
        assert_eq!(XlValueType::from_raw(0x0200), None);
        assert_eq!(XlValueType::from_raw(0x1000), None);
    }

    #[test]
    fn xl_value_type_display_matches_variant_names() {
        assert_eq!(XlValueType::Number.to_string(), "Number");
        assert_eq!(XlValueType::String.to_string(), "String");
        assert_eq!(XlValueType::Boolean.to_string(), "Boolean");
        assert_eq!(XlValueType::Reference.to_string(), "Reference");
        assert_eq!(XlValueType::Error.to_string(), "Error");
        assert_eq!(XlValueType::Flow.to_string(), "Flow");
        assert_eq!(XlValueType::Multi.to_string(), "Multi");
        assert_eq!(XlValueType::Missing.to_string(), "Missing");
        assert_eq!(XlValueType::Nil.to_string(), "Nil");
        assert_eq!(XlValueType::SimpleReference.to_string(), "SimpleReference");
        assert_eq!(XlValueType::Integer.to_string(), "Integer");
        assert_eq!(XlValueType::BigData.to_string(), "BigData");
    }

    #[test]
    fn xl_value_ref_validates_raw_structure_and_extracts_type() {
        let valid_oper = XLOPER12 {
            value: XLOPER12Value { number: 42.0 },
            xltype: XLTYPE_NUM | XLBIT_XL_FREE,
        };
        let value_ref = XlValueRef::from_array_cell(&valid_oper).unwrap();
        assert_eq!(value_ref.value_type(), XlValueType::Number);
        assert_eq!(value_ref.raw_xltype(), XLTYPE_NUM | XLBIT_XL_FREE);
        assert!(!value_ref.is_blank());

        let valid_dll_free_oper = XLOPER12 {
            value: XLOPER12Value { number: 42.0 },
            xltype: XLTYPE_NUM | XLBIT_DLL_FREE,
        };
        let dll_free_ref = XlValueRef::from_array_cell(&valid_dll_free_oper).unwrap();
        assert_eq!(dll_free_ref.value_type(), XlValueType::Number);
        assert_eq!(dll_free_ref.raw_xltype(), XLTYPE_NUM | XLBIT_DLL_FREE);

        let invalid_flag_oper = XLOPER12 {
            value: XLOPER12Value { integer: 0 },
            xltype: XLTYPE_NUM | 0x2000,
        };
        assert!(XlValueRef::from_array_cell(&invalid_flag_oper).is_err());

        let invalid_base_oper = XLOPER12 {
            value: XLOPER12Value { integer: 0 },
            xltype: 0x0003,
        };
        assert!(XlValueRef::from_array_cell(&invalid_base_oper).is_err());

        let nil_oper = XLOPER12::nil();
        let nil_ref = XlValueRef::from_array_cell(&nil_oper).unwrap();
        assert_eq!(nil_ref.value_type(), XlValueType::Nil);
        assert!(nil_ref.is_blank());
    }
}
