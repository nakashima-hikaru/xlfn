//! One process-owned mapping for all loaded copies of the accounting protocol.
//!
//! Each loaded image keeps its own view of the same physical atomic counters.
//! Views remain until process exit because published FP12 allocations may
//! outlive an image. One page of virtual metadata per physical image load is
//! deliberate; no address from shared memory is ever dereferenced.
//!
//! A protected owner/SYSTEM DACL excludes other users and is checked even on
//! pre-existing objects. Cooperating modules and processes with the same user
//! authority are trusted: a DACL cannot authenticate a PID against an adversary
//! with that user's process-memory/handle rights. Corrupt headers and counters
//! fail closed, but this protocol is not isolation from an equal-authority user.

use super::ProcessState;
use crate::{ExcelError, XllError, XllResult};
use core::ffi::c_void;
use std::mem::{align_of, size_of};
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::atomic::Ordering;

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
    let mut mapping = Mapping::open(&name)?;
    let pointer = mapping.view().cast::<ProcessState>();
    if !pointer
        .as_ptr()
        .addr()
        .is_multiple_of(align_of::<ProcessState>())
    {
        return Err(invalid_state("unaligned accounting view"));
    }
    // SAFETY: MapViewOfFile mapped sizeof(ProcessState) writable zero-backed
    // bytes, aligned above. Every field is an AtomicU64 with all bit patterns
    // valid; initialize uses only atomics, including on concurrent attachments.
    let state = unsafe { pointer.as_ref() };
    state.initialize()?;
    // Only this image's MapViewOfFile result is turned into a reference.
    // Close the handle, then retain the view for outstanding process-owned
    // return buffers even if this image is later unloaded.
    mapping.retain_view()?;
    // SAFETY: The owned view has just been retained until process exit.
    Ok(unsafe { pointer.as_ref() })
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

struct Mapping {
    handle: Option<NonNull<c_void>>,
    view: Option<NonNull<c_void>>,
}

impl Mapping {
    fn open(name: &[u16]) -> XllResult<Self> {
        let bytes = u32::try_from(size_of::<ProcessState>())
            .map_err(|_| invalid_state("accounting mapping size exceeds Win32 range"))?;
        let invalid_handle = std::ptr::without_provenance_mut::<c_void>(usize::MAX);
        let security = security::Descriptor::private()?;
        let attributes = security.attributes();
        // SAFETY: INVALID_HANDLE_VALUE selects pagefile-backed memory. The name
        // is NUL-terminated and remains live during the call; attributes and its
        // descriptor remain live, and bytes covers the fixed shared structure.
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
        let handle = NonNull::new(handle).ok_or_else(|| os_error("CreateFileMappingW"))?;
        let mut owner = Self {
            handle: Some(handle),
            view: None,
        };
        security.validate_mapping(handle.as_ptr())?;
        // SAFETY: handle is an owned mapping handle. Offset zero maps exactly
        // ProcessState bytes with coherent read/write access.
        let view = unsafe {
            MapViewOfFile(
                handle.as_ptr(),
                FILE_MAP_WRITE,
                0,
                0,
                size_of::<ProcessState>(),
            )
        };
        owner.view = Some(NonNull::new(view).ok_or_else(|| os_error("MapViewOfFile"))?);
        Ok(owner)
    }

    fn view(&self) -> NonNull<c_void> {
        self.view.expect("an opened accounting mapping has a view")
    }

    fn retain_view(&mut self) -> XllResult<()> {
        if let Some(handle) = self.handle {
            // SAFETY: The mapped view keeps the section alive after closing
            // this owned handle. No other code can close this handle.
            if unsafe { CloseHandle(handle.as_ptr()) } == 0 {
                return Err(os_error("CloseHandle"));
            }
            self.handle = None;
        }
        self.view = None;
        Ok(())
    }

    fn close(&mut self) -> XllResult<()> {
        let mut failure = None;
        if let Some(view) = self.view {
            // SAFETY: This owner still holds the view, and no reference escapes
            // on an error path. Successful attachments retain their own view.
            if unsafe { UnmapViewOfFile(view.as_ptr()) } == 0 {
                failure = Some(os_error("UnmapViewOfFile"));
            } else {
                self.view = None;
            }
        }
        if let Some(handle) = self.handle {
            // SAFETY: This is the owned mapping handle; closing it does not
            // unmap any view or affect other images' handles.
            if unsafe { CloseHandle(handle.as_ptr()) } == 0 {
                let error = os_error("CloseHandle");
                if failure.is_none() {
                    failure = Some(error);
                }
            } else {
                self.handle = None;
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // On construction/validation errors, best-effort cleanup does not
        // replace the original failure. Explicit duplicate cleanup propagates
        // failure and never returns a usable accounting state in that case.
        let _ = self.close();
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
