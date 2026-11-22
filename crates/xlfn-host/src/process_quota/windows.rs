//! One process-owned mapping for all loaded copies of the accounting protocol.
//!
//! Each loaded image keeps its own view of the same physical atomic counters.
//! Views remain until process exit because published FP12 allocations can
//! outlive an image. A shared atomic elects a single section handle to retain
//! the name across all future images or physical reloads; later attachments
//! close their temporary handles. No address from shared memory is dereferenced.
//!
//! A protected owner/SYSTEM DACL excludes other users and is checked even on
//! pre-existing objects. Cooperating modules and processes with the same user
//! authority are trusted: a DACL cannot authenticate a PID against an adversary
//! with that user's process-memory/handle rights. Corrupt headers and counters
//! fail closed, but this protocol is not isolation from an equal-authority user.

use super::ProcessState;
use core::ffi::c_void;
use std::mem::{align_of, size_of};
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::atomic::Ordering;
use xlfn_engine::error::{ExcelError, XllError, XllResult};

mod security;

const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_WRITE: u32 = 0x0002;
#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

windows_link::link!("kernel32.dll" "system" fn CreateFileMappingW(file: *mut c_void, attributes: *const c_void, protect: u32, maximum_high: u32, maximum_low: u32, name: *const u16) -> *mut c_void);
windows_link::link!("kernel32.dll" "system" fn MapViewOfFile(mapping: *mut c_void, access: u32, offset_high: u32, offset_low: u32, bytes: usize) -> *mut c_void);
windows_link::link!("kernel32.dll" "system" fn UnmapViewOfFile(base: *const c_void) -> i32);
windows_link::link!("kernel32.dll" "system" fn CloseHandle(handle: *mut c_void) -> i32);
windows_link::link!("kernel32.dll" "system" fn GetCurrentProcess() -> *mut c_void);
windows_link::link!("kernel32.dll" "system" fn GetCurrentProcessId() -> u32);
windows_link::link!("kernel32.dll" "system" fn GetProcessTimes(process: *mut c_void, creation: *mut FileTime, exit: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32);
windows_link::link!("kernel32.dll" "system" fn GetLastError() -> u32);

pub(super) fn attach() -> XllResult<&'static ProcessState> {
    let name = mapping_name()?;
    Mapping::open(&name)?.initialize_and_retain()
}

fn mapping_name() -> XllResult<Vec<u16>> {
    // SAFETY: These functions take no caller-supplied pointer or ownership.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: GetCurrentProcessId takes no arguments and has no preconditions.
    let pid = unsafe { GetCurrentProcessId() };
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    // SAFETY: The current-process pseudo handle is valid, and the four distinct
    // output structures remain writable for the duration of the call.
    if unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(os_error("GetProcessTimes"));
    }
    let creation = u64::from(creation.low) | (u64::from(creation.high) << 32);
    // PID plus creation time identifies this process incarnation. Keep the
    // protocol version in the header, not the name: incompatible images must
    // fail closed rather than silently establish independent quotas.
    Ok(format!("Local\\xlfn.numeric-returns.{pid}.{creation:016x}")
        .encode_utf16()
        .chain([0])
        .collect())
}

/// A temporary section needs a live handle to remain discoverable by name.
struct MappingHandle(NonNull<c_void>);

impl Drop for MappingHandle {
    fn drop(&mut self) {
        // SAFETY: this is the unique owned handle.
        let _ = unsafe { CloseHandle(self.0.as_ptr()) };
    }
}

/// A complete mapped view. Partial construction is owned by MappingHandle;
/// every value of this type has a real view, including during error cleanup.
struct Mapping {
    view: MappingView,
    handle: MappingHandle,
}

struct MappingView(NonNull<c_void>);

impl Mapping {
    fn open(name: &[u16]) -> XllResult<Self> {
        let bytes = u32::try_from(size_of::<ProcessState>())
            .map_err(|_| invalid_state("accounting mapping size exceeds Win32 range"))?;
        let invalid_handle = std::ptr::without_provenance_mut::<c_void>(usize::MAX);
        let security = security::Descriptor::private()?;
        let attributes = security.attributes();
        // SAFETY: INVALID_HANDLE_VALUE selects pagefile-backed memory. The name
        // and security descriptor remain live for this call.
        let handle = unsafe {
            CreateFileMappingW(
                invalid_handle,
                (&raw const attributes).cast(),
                PAGE_READWRITE,
                0,
                bytes,
                name.as_ptr(),
            )
        };
        let pointer = NonNull::new(handle).ok_or_else(|| os_error("CreateFileMappingW"))?;
        let handle = MappingHandle(pointer);
        security.validate_mapping(pointer.as_ptr())?;
        // SAFETY: the unique handle owns a readable/writable mapping covering
        // the complete fixed-layout ProcessState at offset zero.
        let view = unsafe {
            MapViewOfFile(
                pointer.as_ptr(),
                FILE_MAP_WRITE,
                0,
                0,
                size_of::<ProcessState>(),
            )
        };
        let view = NonNull::new(view).ok_or_else(|| os_error("MapViewOfFile"))?;
        Ok(Self {
            view: MappingView(view),
            handle,
        })
    }

    fn view(&self) -> NonNull<c_void> {
        self.view.0
    }

    /// Validate before transferring both resources to the process lifetime.
    #[allow(
        clippy::mem_forget,
        reason = "FP12 charges require retained views and the process needs one owned section handle"
    )]
    fn initialize_and_retain(self) -> XllResult<&'static ProcessState> {
        let pointer = self.view().cast::<ProcessState>();
        if !pointer
            .as_ptr()
            .addr()
            .is_multiple_of(align_of::<ProcessState>())
        {
            return Err(invalid_state("unaligned accounting view"));
        }
        use crate::win32::{MEM_COMMIT, MEM_MAPPED, MEMORY_BASIC_INFORMATION, VirtualQuery};
        let mut information = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: VirtualQuery inspects an address without dereferencing it;
        // the output is a writable complete MEMORY_BASIC_INFORMATION.
        let queried = unsafe {
            VirtualQuery(
                self.view().as_ptr(),
                &mut information,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if queried != size_of::<MEMORY_BASIC_INFORMATION>()
            || information.BaseAddress != self.view().as_ptr()
            || information.RegionSize < size_of::<ProcessState>()
            || information.State != MEM_COMMIT as u32
            || information.Type != MEM_MAPPED as u32
            || information.Protect != PAGE_READWRITE
        {
            return Err(invalid_state(
                "accounting view is not committed read/write mapped memory",
            ));
        }
        // SAFETY: the view owns sizeof(ProcessState) writable bytes, aligned
        // above. Its atomic fields accept all bit patterns; initialization
        // never resets live accounting from another attachment.
        let state = unsafe { pointer.as_ref() };
        state.initialize()?;
        let anchors_section = state.claim_section_anchor()?;
        let Self { view, handle } = self;
        // Winning the election commits the valid handle to process lifetime.
        // There is no fallible step between election and this transfer. A
        // losing attachment can close its temporary handle: the elected
        // owner already holds a separate handle on the same section.
        if anchors_section {
            std::mem::forget(handle);
        }
        std::mem::forget(view);
        // SAFETY: the exact initialized view was just permanently retained.
        Ok(unsafe { pointer.as_ref() })
    }
}

impl Drop for MappingView {
    fn drop(&mut self) {
        // SAFETY: this owner retains its view and has issued no escaping
        // reference. An unmapping failure does not transfer handle ownership.
        let _ = unsafe { UnmapViewOfFile(self.0.as_ptr()) };
    }
}

fn invalid_state(message: &'static str) -> XllError {
    XllError::custom(ExcelError::Number, message)
}

fn os_error(operation: &'static str) -> XllError {
    // SAFETY: GetLastError takes no arguments and observes this thread's error.
    let code = unsafe { GetLastError() };
    XllError::custom(
        ExcelError::Number,
        format!("FP12 process accounting: {operation} failed (Windows error {code})"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_views_share_physical_counters_and_survive_detach() {
        let name = format!(
            "Local\\xlfn.numeric-test.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
        let first = Mapping::open(&name).unwrap();
        let second = Mapping::open(&name).unwrap();
        assert_ne!(first.view(), second.view());
        // SAFETY: Both owned, aligned views cover a full ProcessState of atomics.
        let a = unsafe { first.view().cast::<ProcessState>().as_ref() };
        // SAFETY: The independent second view remains live through this scope.
        let b = unsafe { second.view().cast::<ProcessState>().as_ref() };
        a.initialize().unwrap();
        b.initialize().unwrap();
        a.reserve(48, 80).unwrap();
        a.publish(48);
        assert_eq!(b.published_bytes.load(Ordering::Relaxed), 48);
        assert!(b.reserve(33, 80).is_err());
        drop(second);
        let reloaded = Mapping::open(&name).unwrap();
        // SAFETY: reloaded owns a complete writable view until scope exit.
        let c = unsafe { reloaded.view().cast::<ProcessState>().as_ref() };
        c.initialize().unwrap();
        assert_eq!(c.reserved_bytes.load(Ordering::Relaxed), 48);
        c.release_published(48);
        c.reserve(80, 80).unwrap();
        c.release_reserved(80);
        assert_eq!(a.reserved_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn retained_mapping_can_be_found_by_a_later_attachment() {
        let name = format!(
            "Local\\xlfn.numeric-retained-test.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
        let first = Mapping::open(&name)
            .unwrap()
            .initialize_and_retain()
            .unwrap();
        first.reserve(48, 80).unwrap();
        first.publish(48);
        let later = Mapping::open(&name)
            .unwrap()
            .initialize_and_retain()
            .unwrap();
        let published = later.published_bytes.load(Ordering::Relaxed);
        first.release_published(48);
        assert_eq!(published, 48);
        assert_eq!(later.published_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn concurrent_attachments_share_the_same_header() {
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    attach().unwrap().validate().unwrap();
                });
            }
        });
    }
}

#[cfg(test)]
mod reserved_mapping_tests {
    use super::*;
    #[test]
    fn reserved_uncommitted_mapping_is_rejected_before_access() {
        let name = format!(
            "Local\\xlfn.reserved.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
        let descriptor = security::Descriptor::private().unwrap();
        let attributes = descriptor.attributes();
        // SAFETY: live name and security descriptor create a pagefile section;
        // SEC_RESERVE deliberately leaves its pages uncommitted.
        let handle = unsafe {
            CreateFileMappingW(
                std::ptr::without_provenance_mut(usize::MAX),
                (&raw const attributes).cast(),
                PAGE_READWRITE | 0x04000000,
                0,
                4096,
                name.as_ptr(),
            )
        };
        assert!(!handle.is_null());
        let result = Mapping::open(&name).and_then(Mapping::initialize_and_retain);
        // SAFETY: this test owns the section handle and has released its view.
        unsafe { CloseHandle(handle) };
        assert!(result.is_err());
    }
}
