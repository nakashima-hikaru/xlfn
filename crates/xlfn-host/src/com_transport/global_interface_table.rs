use crate::win32::{CLSCTX_INPROC_SERVER, CoCreateInstance, E_POINTER, GUID};
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
pub struct GlobalInterfaceTable {
    pointer: NonNull<IGlobalInterfaceTable>,
}

/// # Safety
/// The caller must keep the current COM apartment entered until the owned
/// interface is dropped.
pub unsafe fn get_git() -> Result<GlobalInterfaceTable, i32> {
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

    if status < 0 {
        Err(status)
    } else {
        NonNull::new(git.cast())
            .map(|pointer| GlobalInterfaceTable { pointer })
            .ok_or(E_POINTER)
    }
}

impl GlobalInterfaceTable {
    fn vtable(&self) -> &IGlobalInterfaceTableVtable {
        // SAFETY: the owned COM reference keeps this interface live.
        let table = unsafe { self.pointer.as_ref() }.vtable;
        // SAFETY: COM supplies a live IGlobalInterfaceTable vtable for the
        // lifetime of the interface.
        unsafe { &*table }
    }

    /// # Safety
    /// The caller must be in the COM apartment that owns this interface.
    pub unsafe fn revoke(&self, cookie: u32) -> i32 {
        // SAFETY: the wrapper owns a live GIT interface and the vtable method
        // receives the same interface pointer returned by CoCreateInstance.
        unsafe { (self.vtable().revoke_interface_from_global)(self.pointer.as_ptr(), cookie) }
    }

    /// # Safety
    /// `callback`, `interface_id`, and `cookie` must satisfy the GIT method's
    /// COM contract and remain live for the duration of this call.
    pub unsafe fn register(
        &self,
        callback: *mut c_void,
        interface_id: *const GUID,
        cookie: *mut u32,
    ) -> i32 {
        // SAFETY: validated by this method's caller contract.
        unsafe {
            (self.vtable().register_interface_in_global)(
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
    pub unsafe fn get_interface(
        &self,
        cookie: u32,
        interface_id: *const GUID,
    ) -> Result<NonNull<c_void>, i32> {
        let mut output = ptr::null_mut();
        // SAFETY: `interface_id` is validated by this method's caller contract
        // and `output` is a writable local result slot.
        let status = unsafe {
            (self.vtable().get_interface_from_global)(
                self.pointer.as_ptr(),
                cookie,
                interface_id,
                &mut output,
            )
        };

        if status < 0 {
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
            (self.vtable().release)(self.pointer.as_ptr());
        }
    }
}
