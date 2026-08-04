//! C-compatible Excel 12 value and constant definitions.

#![allow(
    non_camel_case_types,
    non_snake_case,
    reason = "C C-ABI SDK type names"
)]

use core::fmt;

/// Excel 12 row index.
pub type RW = i32;
/// Excel 12 column index.
pub type COL = i32;
/// Excel 12 sheet identifier. `XLCALL.H` defines this as `DWORD_PTR`.
pub type IDSHEET = usize;
/// Excel UTF-16 code unit.
pub type XCHAR = u16;

pub const XLTYPE_NUM: u32 = 0x0001;
pub const XLTYPE_STR: u32 = 0x0002;
pub const XLTYPE_BOOL: u32 = 0x0004;
pub const XLTYPE_REF: u32 = 0x0008;
pub const XLTYPE_ERR: u32 = 0x0010;
pub const XLTYPE_FLOW: u32 = 0x0020;
pub const XLTYPE_MULTI: u32 = 0x0040;
pub const XLTYPE_MISSING: u32 = 0x0080;
pub const XLTYPE_NIL: u32 = 0x0100;
pub const XLTYPE_SREF: u32 = 0x0400;
pub const XLTYPE_INT: u32 = 0x0800;
pub const XLTYPE_BIG_DATA: u32 = XLTYPE_STR | XLTYPE_INT;
pub const XLTYPE_MASK: u32 = 0x0fff;

pub const XLBIT_XL_FREE: u32 = 0x1000;
pub const XLBIT_DLL_FREE: u32 = 0x4000;

pub const XLERR_NULL: i32 = 0;
pub const XLERR_DIV0: i32 = 7;
pub const XLERR_VALUE: i32 = 15;
pub const XLERR_REF: i32 = 23;
pub const XLERR_NAME: i32 = 29;
pub const XLERR_NUM: i32 = 36;
pub const XLERR_NA: i32 = 42;
pub const XLERR_GETTING_DATA: i32 = 43;

pub const XL_FREE: i32 = 0x4000;
pub const XL_COERCE: i32 = 0x4002;
pub const XL_SHEET_ID: i32 = 0x4004;
pub const XL_SHEET_NM: i32 = 0x4005;
pub const XL_GET_INST: i32 = 0x4007;
pub const XL_GET_NAME: i32 = 0x4009;
pub const XL_GET_INST_PTR: i32 = 0x4013;
pub const XL_ASYNC_RETURN: i32 = 0x4010;
pub const XL_EVENT_REGISTER: i32 = 0x4011;
pub const XLEVENT_CALCULATION_ENDED: i32 = 1;
pub const XLEVENT_CALCULATION_CANCELED: i32 = 2;
pub const XLRET_INV_ASYNC_CONTEXT: i32 = 256;
pub const XLF_SET_NAME: i32 = 88;
pub const XLF_CALLER: i32 = 89;
pub const XLF_REGISTER: i32 = 149;
pub const XLF_UNREGISTER: i32 = 201;
pub const XLF_EVALUATE: i32 = 257;
pub const XLF_RTD: i32 = 379;

pub const XLFLOW_HALT: u8 = 1;
pub const XLFLOW_GOTO: u8 = 2;
pub const XLFLOW_RESTART: u8 = 8;
pub const XLFLOW_PAUSE: u8 = 16;
pub const XLFLOW_RESUME: u8 = 64;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLREF12 {
    pub rw_first: RW,
    pub rw_last: RW,
    pub col_first: COL,
    pub col_last: COL,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLMREF12 {
    pub count: u16,
    pub reftbl: [XLREF12; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLOPER12SRef {
    pub count: u16,
    pub reference: XLREF12,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLOPER12MRef {
    pub references: *mut XLMREF12,
    pub sheet_id: IDSHEET,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLOPER12Array {
    pub values: *mut XLOPER12,
    pub rows: RW,
    pub columns: COL,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union XLOPER12FlowValue {
    pub level: i32,
    pub toolbar_control: i32,
    pub sheet_id: IDSHEET,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLOPER12Flow {
    pub value: XLOPER12FlowValue,
    pub row: RW,
    pub column: COL,
    pub flow: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union XLOPER12BigDataHandle {
    pub data: *mut u8,
    pub handle: *mut core::ffi::c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLOPER12BigData {
    pub handle: XLOPER12BigDataHandle,
    pub byte_count: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union XLOPER12Value {
    pub number: f64,
    pub string: *mut XCHAR,
    pub boolean: i32,
    pub error: i32,
    pub integer: i32,
    pub sref: XLOPER12SRef,
    pub mref: XLOPER12MRef,
    pub array: XLOPER12Array,
    pub flow: XLOPER12Flow,
    pub big_data: XLOPER12BigData,
}

/// Excel's fundamental Unicode value carrier.
///
/// The layout is 32 bytes with 8-byte alignment on both supported MSVC
/// targets. The ABI probes compare this definition with Microsoft's
/// `XLCALL.H` at build time on Windows.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLOPER12 {
    pub value: XLOPER12Value,
    pub xltype: u32,
}

// These compile-time checks run for every target, including cross-target
// cargo check where the C++ ABI probe cannot execute.
static_assertions::const_assert_eq!(core::mem::size_of::<XLOPER12>(), 32);
static_assertions::const_assert_eq!(core::mem::align_of::<XLOPER12>(), 8);
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12, xltype), 24);
static_assertions::const_assert_eq!(core::mem::size_of::<XLOPER12SRef>(), 20);
static_assertions::const_assert_eq!(core::mem::size_of::<XCHAR>(), 2);
static_assertions::const_assert_eq!(
    core::mem::size_of::<IDSHEET>(),
    core::mem::size_of::<usize>()
);
static_assertions::const_assert_eq!(
    core::mem::size_of::<XLOPER12FlowValue>(),
    core::mem::size_of::<usize>()
);

#[cfg(target_pointer_width = "32")]
static_assertions::const_assert_eq!(core::mem::size_of::<XLOPER12MRef>(), 8);
#[cfg(target_pointer_width = "32")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12MRef, sheet_id), 4);
#[cfg(target_pointer_width = "32")]
static_assertions::const_assert_eq!(core::mem::size_of::<XLOPER12Flow>(), 16);
#[cfg(target_pointer_width = "32")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12Flow, row), 4);
#[cfg(target_pointer_width = "32")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12Flow, column), 8);
#[cfg(target_pointer_width = "32")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12Flow, flow), 12);

#[cfg(target_pointer_width = "64")]
static_assertions::const_assert_eq!(core::mem::size_of::<XLOPER12MRef>(), 16);
#[cfg(target_pointer_width = "64")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12MRef, sheet_id), 8);
#[cfg(target_pointer_width = "64")]
static_assertions::const_assert_eq!(core::mem::size_of::<XLOPER12Flow>(), 24);
#[cfg(target_pointer_width = "64")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12Flow, row), 8);
#[cfg(target_pointer_width = "64")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12Flow, column), 12);
#[cfg(target_pointer_width = "64")]
static_assertions::const_assert_eq!(core::mem::offset_of!(XLOPER12Flow, flow), 16);

impl XLOPER12 {
    #[must_use]
    pub const fn nil() -> Self {
        Self {
            value: XLOPER12Value { integer: 0 },
            xltype: XLTYPE_NIL,
        }
    }

    #[must_use]
    pub const fn missing() -> Self {
        Self {
            value: XLOPER12Value { integer: 0 },
            xltype: XLTYPE_MISSING,
        }
    }

    #[must_use]
    pub const fn number(value: f64) -> Self {
        Self {
            value: XLOPER12Value { number: value },
            xltype: XLTYPE_NUM,
        }
    }

    #[must_use]
    pub const fn integer(value: i32) -> Self {
        Self {
            value: XLOPER12Value { integer: value },
            xltype: XLTYPE_INT,
        }
    }

    #[must_use]
    pub const fn boolean(value: bool) -> Self {
        Self {
            value: XLOPER12Value {
                boolean: value as i32,
            },
            xltype: XLTYPE_BOOL,
        }
    }

    #[must_use]
    pub const fn error(value: i32) -> Self {
        Self {
            value: XLOPER12Value { error: value },
            xltype: XLTYPE_ERR,
        }
    }

    #[must_use]
    pub const fn base_type(&self) -> u32 {
        self.xltype & XLTYPE_MASK
    }
}

impl Default for XLOPER12 {
    fn default() -> Self {
        Self::nil()
    }
}

impl fmt::Debug for XLOPER12 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("XLOPER12")
            .field("xltype", &format_args!("{:#06x}", self.xltype))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn xloper12_layout_matches_excel_sdk() {
        assert_eq!(size_of::<XLREF12>(), 16);
        assert_eq!(offset_of!(XLOPER12SRef, reference), 4);
        assert_eq!(size_of::<XLOPER12SRef>(), 20);
        assert_eq!(size_of::<IDSHEET>(), size_of::<usize>());
        assert_eq!(size_of::<XLOPER12FlowValue>(), size_of::<usize>());
        #[cfg(target_pointer_width = "32")]
        {
            assert_eq!(size_of::<XLOPER12Flow>(), 16);
            assert_eq!(offset_of!(XLOPER12Flow, row), 4);
        }
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(size_of::<XLOPER12Flow>(), 24);
            assert_eq!(offset_of!(XLOPER12Flow, row), 8);
        }
        assert_eq!(offset_of!(XLOPER12, xltype), 24);
        assert_eq!(size_of::<XLOPER12>(), 32);
        assert_eq!(align_of::<XLOPER12>(), 8);
    }

    #[test]
    fn constructors_set_exact_root_type() {
        assert_eq!(XLOPER12::number(1.0).base_type(), XLTYPE_NUM);
        assert_eq!(XLOPER12::boolean(true).base_type(), XLTYPE_BOOL);
        assert_eq!(XLOPER12::integer(1).base_type(), XLTYPE_INT);
        assert_eq!(XLOPER12::missing().base_type(), XLTYPE_MISSING);
        assert_eq!(XLOPER12::nil().base_type(), XLTYPE_NIL);
    }
}
