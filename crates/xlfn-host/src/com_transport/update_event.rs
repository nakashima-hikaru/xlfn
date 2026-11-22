use std::ffi::c_void;
use std::ptr::NonNull;

#[repr(C)]
pub struct RtdUpdateEvent {
    vtable: *const RtdUpdateEventVtable,
}

#[repr(C)]
struct RtdUpdateEventVtable {
    query_interface: unsafe extern "system" fn(
        *mut RtdUpdateEvent,
        *const crate::win32::GUID,
        *mut *mut c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(*mut RtdUpdateEvent) -> u32,
    release: unsafe extern "system" fn(*mut RtdUpdateEvent) -> u32,
    get_type_info_count: unsafe extern "system" fn(*mut RtdUpdateEvent, *mut u32) -> i32,
    get_type_info:
        unsafe extern "system" fn(*mut RtdUpdateEvent, u32, u32, *mut *mut c_void) -> i32,
    get_ids_of_names: unsafe extern "system" fn(
        *mut RtdUpdateEvent,
        *const crate::win32::GUID,
        *const *const u16,
        u32,
        u32,
        *mut i32,
    ) -> i32,
    invoke: unsafe extern "system" fn(
        *mut RtdUpdateEvent,
        i32,
        *const crate::win32::GUID,
        u32,
        u16,
        *mut c_void,
        *mut crate::win32::VARIANT,
        *mut c_void,
        *mut u32,
    ) -> i32,
    update_notify: unsafe extern "system" fn(*mut RtdUpdateEvent) -> i32,
    get_heartbeat_interval: unsafe extern "system" fn(*mut RtdUpdateEvent, *mut i32) -> i32,
    set_heartbeat_interval: unsafe extern "system" fn(*mut RtdUpdateEvent, i32) -> i32,
    disconnect: unsafe extern "system" fn(*mut RtdUpdateEvent) -> i32,
}

/// Owns one `IRTDUpdateEvent` reference returned by either COM
/// `QueryInterface` or the Global Interface Table.
pub struct OwnedRtdUpdateEvent {
    pointer: NonNull<RtdUpdateEvent>,
}

impl OwnedRtdUpdateEvent {
    fn vtable(&self) -> &RtdUpdateEventVtable {
        // SAFETY: the owned COM reference keeps this interface live.
        let table = unsafe { self.pointer.as_ref() }.vtable;
        // SAFETY: COM supplies the dispatch table for the interface lifetime.
        unsafe { &*table }
    }

    /// # Safety
    /// `pointer` must identify a live `IRTDUpdateEvent` interface and own one
    /// COM reference that this value can release exactly once. Keep the owning
    /// apartment entered until this value is dropped.
    pub unsafe fn from_raw(pointer: NonNull<RtdUpdateEvent>) -> Self {
        Self { pointer }
    }

    pub fn as_ptr(&self) -> *mut RtdUpdateEvent {
        self.pointer.as_ptr()
    }

    /// # Safety
    /// The caller must remain in the COM apartment for this interface and
    /// hold any server/module admission required to invoke the callback.
    pub unsafe fn notify(&self) -> i32 {
        // SAFETY: the wrapper owns a live interface reference for this call.
        unsafe { (self.vtable().update_notify)(self.pointer.as_ptr()) }
    }
}

impl Drop for OwnedRtdUpdateEvent {
    fn drop(&mut self) {
        // SAFETY: `pointer` is a live interface with exactly one owned
        // reference, and Drop runs exactly once.
        unsafe {
            (self.vtable().release)(self.pointer.as_ptr());
        }
    }
}
