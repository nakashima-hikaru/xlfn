use crate::win32::{CLSCTX_INPROC_SERVER, CoCreateInstance, E_POINTER, GUID, S_OK};
use std::ffi::c_void;
use std::ptr::{self, NonNull};

const CLSID_STD_GLOBAL_INTERFACE_TABLE: GUID = GUID {
    data1: 0x00000323,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};
const IID_IGLOBAL_INTERFACE_TABLE: GUID = GUID {
    data1: 0x00000146,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

#[repr(C)]
struct IGlobalInterfaceTable {
    vtable: *const IGlobalInterfaceTableVtable,
}

#[repr(C)]
struct IGlobalInterfaceTableVtable {
    query_interface:
        unsafe extern "system" fn(*mut IGlobalInterfaceTable, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IGlobalInterfaceTable) -> u32,
    release: unsafe extern "system" fn(*mut IGlobalInterfaceTable) -> u32,
    register_interface_in_global: unsafe extern "system" fn(
        *mut IGlobalInterfaceTable,
        *mut c_void,
        *const GUID,
        *mut u32,
    ) -> i32,
    revoke_interface_from_global: unsafe extern "system" fn(*mut IGlobalInterfaceTable, u32) -> i32,
    get_interface_from_global: unsafe extern "system" fn(
        *mut IGlobalInterfaceTable,
        u32,
        *const GUID,
        *mut *mut c_void,
    ) -> i32,
}

/// Owns the COM reference returned by CoCreateInstance for the GIT.
pub(super) struct GlobalInterfaceTable {
    pointer: NonNull<IGlobalInterfaceTable>,
}

/// # Safety
/// The caller must have entered a COM apartment on the current thread.
pub(super) unsafe fn get_git() -> Result<GlobalInterfaceTable, i32> {
    let mut git: *mut c_void = ptr::null_mut();

    // SAFETY: `git` is a valid writable output slot, the aggregation pointer is
    // null as required for this COM class, and both GUID pointers identify the
    // standard Global Interface Table class and interface.
    let status = unsafe {
        CoCreateInstance(
            &CLSID_STD_GLOBAL_INTERFACE_TABLE,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IGLOBAL_INTERFACE_TABLE,
            &mut git,
        )
    };

    if status == S_OK {
        NonNull::new(git.cast())
            .map(|pointer| GlobalInterfaceTable { pointer })
            .ok_or(status)
    } else {
        Err(status)
    }
}

impl GlobalInterfaceTable {
    /// # Safety
    /// The caller must be in the COM apartment that owns this interface.
    pub(super) unsafe fn revoke(&self, cookie: u32) -> i32 {
        // SAFETY: the wrapper owns a live GIT interface and the vtable method
        // receives the same interface pointer returned by CoCreateInstance.
        unsafe {
            ((*(*self.pointer.as_ptr()).vtable).revoke_interface_from_global)(
                self.pointer.as_ptr(),
                cookie,
            )
        }
    }

    /// # Safety
    /// `callback`, `interface_id`, and `cookie` must satisfy the GIT method's
    /// COM contract and remain live for the duration of this call.
    pub(super) unsafe fn register(
        &self,
        callback: *mut c_void,
        interface_id: *const GUID,
        cookie: *mut u32,
    ) -> i32 {
        // SAFETY: validated by this method's caller contract.
        unsafe {
            ((*(*self.pointer.as_ptr()).vtable).register_interface_in_global)(
                self.pointer.as_ptr(),
                callback,
                interface_id,
                cookie,
            )
        }
    }

    /// # Safety
    /// `interface_id` must point to a readable GUID for the duration of the
    /// call. The returned pointer owns the one interface reference produced by
    /// GetInterfaceFromGlobal.
    pub(super) unsafe fn get_interface(
        &self,
        cookie: u32,
        interface_id: *const GUID,
    ) -> Result<NonNull<c_void>, i32> {
        let mut output = ptr::null_mut();
        // SAFETY: `interface_id` is validated by this method's caller contract
        // and `output` is a writable local result slot.
        let status = unsafe {
            ((*(*self.pointer.as_ptr()).vtable).get_interface_from_global)(
                self.pointer.as_ptr(),
                cookie,
                interface_id,
                &mut output,
            )
        };

        if status != S_OK {
            return Err(status);
        }

        NonNull::new(output).ok_or(E_POINTER)
    }
}

impl Drop for GlobalInterfaceTable {
    fn drop(&mut self) {
        // SAFETY: `pointer` owns the one COM reference returned by
        // CoCreateInstance and Drop runs exactly once.
        unsafe {
            ((*(*self.pointer.as_ptr()).vtable).release)(self.pointer.as_ptr());
        }
    }
}
