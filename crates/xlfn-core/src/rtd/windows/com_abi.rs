use crate::win32::GUID;
use std::ffi::c_void;

pub(super) const IID_IUNKNOWN: GUID = GUID::from_u128(0x0000_0000_0000_0000_c000_0000_0000_0046);

impl Default for GUID {
    fn default() -> Self {
        Self::from_u128(0)
    }
}

#[repr(C)]
#[allow(non_snake_case)]
pub(super) struct IUnknown_Vtbl {
    pub QueryInterface: unsafe extern "system" fn(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> i32,
    pub AddRef: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}
