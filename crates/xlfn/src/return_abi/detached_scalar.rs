//! Primitive worksheet returns with no DLL-owned cleanup obligation.
//!
//! Excel reads the root before calling another worksheet function on the same
//! thread. A separate root per thread therefore needs no `xlAutoFree12` call.
//! The root is allocated outside the module on Windows and deliberately kept
//! for the process lifetime: physical unload cannot invalidate a late Excel
//! read, and a user-supplied Rust allocator does not own this ABI memory.
//!
//! Cost: one 32-byte root per thread that publishes a primitive, per loaded
//! module instance. It has no payload, destructor, or executable callback.

use std::cell::Cell;
use std::ptr::NonNull;
use xlfn_sys::XLOPER12;

thread_local! {
    // Only the pointer is thread-local. The pointee survives both thread exit
    // and physical module unload; no TLS destructor can enter unloaded code.
    static ROOT: Cell<*mut XLOPER12> = const { Cell::new(std::ptr::null_mut()) };
}

const _: () = assert!(!std::mem::needs_drop::<Cell<*mut XLOPER12>>());

pub(super) fn publish(value: XLOPER12) -> *mut XLOPER12 {
    debug_assert!(matches!(
        value.base_type(),
        xlfn_sys::XLTYPE_NUM | xlfn_sys::XLTYPE_BOOL | xlfn_sys::XLTYPE_ERR
    ));
    debug_assert_eq!(value.xltype & xlfn_sys::XLBIT_DLL_FREE, 0);
    ROOT.with(|root| {
        let mut pointer = root.get();
        if pointer.is_null() {
            pointer = allocate_root();
            root.set(pointer);
        }
        // SAFETY: Each thread owns its own process-lifetime root. Excel has
        // consumed the previous result before its next call on this thread,
        // and publication happens only after all nested UDF work has returned.
        unsafe { pointer.write(value) };
        pointer
    })
}

pub(super) struct PermanentRoot(NonNull<XLOPER12>);

// SAFETY: PermanentRoot is constructed only from a primitive in process-owned
// memory, with no payload or cleanup callback. Publication initializes it once;
// every access after construction is read-only, including Excel's ABI reads.
unsafe impl Send for PermanentRoot {}
// SAFETY: The same immutable, process-lifetime value may be read by all threads.
unsafe impl Sync for PermanentRoot {}

impl PermanentRoot {
    pub(super) fn pointer(&self) -> *mut XLOPER12 {
        self.0.as_ptr()
    }
}

/// Allocates a root that will never be rewritten or released by the module.
pub(super) fn permanent(value: XLOPER12) -> PermanentRoot {
    debug_assert!(matches!(
        value.base_type(),
        xlfn_sys::XLTYPE_NUM | xlfn_sys::XLTYPE_BOOL | xlfn_sys::XLTYPE_ERR
    ));
    let pointer = allocate_root();
    // SAFETY: The new process-owned root is exclusive until returned, and
    // callers keep this initialized value immutable for the process lifetime.
    unsafe { pointer.write(value) };
    PermanentRoot(NonNull::new(pointer).expect("root allocation never returns null"))
}

#[cfg(target_os = "windows")]
fn allocate_root() -> *mut XLOPER12 {
    windows_link::link!("kernel32.dll" "system" fn GetProcessHeap() -> *mut core::ffi::c_void);
    windows_link::link!("kernel32.dll" "system" fn HeapAlloc(heap: *mut core::ffi::c_void, flags: u32, bytes: usize) -> *mut core::ffi::c_void);
    // SAFETY: GetProcessHeap has no preconditions.
    let heap = unsafe { GetProcessHeap() };
    // SAFETY: HeapAlloc receives the valid process heap and enough suitably
    // aligned bytes for one XLOPER12.
    let pointer: *mut XLOPER12 = unsafe { HeapAlloc(heap, 0, size_of::<XLOPER12>()) }.cast();
    if pointer.is_null() {
        std::alloc::handle_alloc_error(std::alloc::Layout::new::<XLOPER12>());
    }
    pointer
}

// Non-Windows builds are deterministic tests and benchmarks, rather than
// unloadable Excel XLLs. Retaining the allocation gives them the same ABI
// lifetime and thread-exit behavior as the Windows process heap.
#[cfg(not(target_os = "windows"))]
fn allocate_root() -> *mut XLOPER12 {
    Box::into_raw(Box::new(XLOPER12::nil()))
}
