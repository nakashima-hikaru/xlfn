use crate::XllError;
use crate::win32::{CO_E_SERVER_STOPPING, E_UNEXPECTED, GUID};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::module_lifetime;

pub(crate) const IID_IUNKNOWN: GUID = GUID::from_u128(0x0000_0000_0000_0000_c000_0000_0000_0046);

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

impl Default for GUID {
    fn default() -> Self {
        Self::from_u128(0)
    }
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
