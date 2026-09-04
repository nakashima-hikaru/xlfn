use crate::XllError;
use crate::win32::{CO_E_SERVER_STOPPING, E_UNEXPECTED, GUID};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::module_lifetime;

// COM VARIANT vt (VARTYPE = u16) constants.
//
// In Windows metadata, `VARENUM` is typed as an i32 enum while `VARIANT::vt` and
// `SafeArrayCreate` expect `VARTYPE` (u16). We provide typed u16 constants here
// for ergonomic and type-safe COM VARIANT manipulation without pervasive casts.
pub(crate) const VT_EMPTY: u16 = crate::win32::VT_EMPTY as u16;
pub(crate) const VT_I4: u16 = crate::win32::VT_I4 as u16;
pub(crate) const VT_R8: u16 = crate::win32::VT_R8 as u16;
pub(crate) const VT_BSTR: u16 = crate::win32::VT_BSTR as u16;
pub(crate) const VT_BOOL: u16 = crate::win32::VT_BOOL as u16;
pub(crate) const VT_ERROR: u16 = crate::win32::VT_ERROR as u16;
pub(crate) const VT_DISPATCH: u16 = crate::win32::VT_DISPATCH as u16;
pub(crate) const VT_UNKNOWN: u16 = crate::win32::VT_UNKNOWN as u16;
pub(crate) const VT_VARIANT: u16 = crate::win32::VT_VARIANT as u16;
pub(crate) const VT_ARRAY: u16 = crate::win32::VT_ARRAY as u16;
pub(crate) const VT_BYREF: u16 = crate::win32::VT_BYREF as u16;

pub(crate) const IID_IUNKNOWN: GUID = crate::win32::IID_IUnknown;

pub(crate) fn com_boundary(operation: &'static str, callback: impl FnOnce() -> i32) -> i32 {
    let (_module_call, accepted) = module_lifetime().enter_call();
    if !accepted {
        return CO_E_SERVER_STOPPING;
    }
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(status) => status,
        Err(_) => {
            crate::diagnostics::report_no_unwind(operation, &XllError::Panic);
            E_UNEXPECTED
        }
    }
}

pub(crate) fn guid_eq(left: GUID, right: GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

#[repr(C)]
#[allow(non_snake_case, reason = "COM IUnknown vtable ABI method names")]
pub(super) struct IUnknown_Vtbl {
    pub QueryInterface: unsafe extern "system" fn(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> i32,
    pub AddRef: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}
