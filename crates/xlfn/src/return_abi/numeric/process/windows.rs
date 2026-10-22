//! One process-owned mapping for all loaded copies of the accounting protocol.
//!
//! The canonical view and its mapping handle are deliberately retained until
//! process exit. Neither the view nor its atomic contents points into an XLL.
//! Later attachments discard their temporary view and handle after finding the
//! canonical address, so physical reloads do not add retained metadata.

use super::ProcessState;
use crate::{ExcelError, XllError, XllResult};
use core::ffi::c_void;
use std::mem::{MaybeUninit, align_of, size_of};
use std::ptr::NonNull;
use std::sync::atomic::Ordering;

const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_WRITE: u32 = 0x0002;
const MEM_COMMIT: u32 = 0x1000;
const MEM_MAPPED: u32 = 0x40000;

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

// The native SDK structure has no PartitionId member on x86. On 64-bit
// Windows its u16 member occupies part of the padding before RegionSize.
// https://learn.microsoft.com/windows/win32/api/winnt/ns-winnt-memory_basic_information
#[repr(C)]
struct MemoryBasicInformation {
    base_address: *mut c_void,
    allocation_base: *mut c_void,
    allocation_protect: u32,
    #[cfg(target_pointer_width = "64")]
    partition_id: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

#[cfg(target_pointer_width = "32")]
const _: () = assert!(size_of::<MemoryBasicInformation>() == 28);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MemoryBasicInformation>() == 48);

windows_link::link!("kernel32.dll" "system" fn CreateFileMappingW(file: *mut c_void, attributes: *const c_void, protect: u32, maximum_high: u32, maximum_low: u32, name: *const u16) -> *mut c_void);
windows_link::link!("kernel32.dll" "system" fn MapViewOfFile(mapping: *mut c_void, access: u32, offset_high: u32, offset_low: u32, bytes: usize) -> *mut c_void);
windows_link::link!("kernel32.dll" "system" fn UnmapViewOfFile(base: *const c_void) -> i32);
windows_link::link!("kernel32.dll" "system" fn CloseHandle(handle: *mut c_void) -> i32);
windows_link::link!("kernel32.dll" "system" fn GetCurrentProcess() -> *mut c_void);
windows_link::link!("kernel32.dll" "system" fn GetCurrentProcessId() -> u32);
windows_link::link!("kernel32.dll" "system" fn GetProcessTimes(process: *mut c_void, creation: *mut FileTime, exit: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32);
windows_link::link!("kernel32.dll" "system" fn GetLastError() -> u32);
windows_link::link!("kernel32.dll" "system" fn VirtualQuery(address: *const c_void, information: *mut MemoryBasicInformation, bytes: usize) -> usize);

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
    let address = pointer.as_ptr().expose_provenance() as u64;
    match state
        .canonical_view
        .compare_exchange(0, address, Ordering::AcqRel, Ordering::Acquire)
    {
        Ok(_) => {
            // Publish before forgetting ownership: any concurrent attachment
            // sees this still-live mapping, and this branch cannot fail.
            mapping.retain();
            // SAFETY: The canonical view and its handle now remain allocated
            // until process exit; the mapping contains atomics and no XLL data.
            Ok(unsafe { pointer.as_ref() })
        }
        Err(canonical) => {
            let canonical = validate_canonical(canonical, state)?;
            mapping.close()?;
            Ok(canonical)
        }
    }
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

fn validate_canonical(address: u64, attached: &ProcessState) -> XllResult<&'static ProcessState> {
    let address = usize::try_from(address)
        .map_err(|_| invalid_state("accounting address exceeds pointer width"))?;
    if address == 0 || !address.is_multiple_of(align_of::<ProcessState>()) {
        return Err(invalid_state("invalid canonical accounting address"));
    }
    let pointer = std::ptr::with_exposed_provenance_mut::<ProcessState>(address);
    let mut information = MaybeUninit::<MemoryBasicInformation>::uninit();
    // SAFETY: VirtualQuery inspects an address without dereferencing it in Rust;
    // information provides exactly the native structure's writable extent.
    let written = unsafe {
        VirtualQuery(
            pointer.cast(),
            information.as_mut_ptr(),
            size_of::<MemoryBasicInformation>(),
        )
    };
    if written != size_of::<MemoryBasicInformation>() {
        return Err(os_error("VirtualQuery"));
    }
    // SAFETY: VirtualQuery reported that it initialized the complete structure.
    let information = unsafe { information.assume_init() };
    let end = address
        .checked_add(size_of::<ProcessState>())
        .ok_or_else(|| invalid_state("canonical accounting extent overflow"))?;
    let region_end = information
        .base_address
        .addr()
        .checked_add(information.region_size)
        .ok_or_else(|| invalid_state("canonical accounting region overflow"))?;
    if information.state != MEM_COMMIT
        || information.kind != MEM_MAPPED
        || information.protect != PAGE_READWRITE
        || information.allocation_protect != PAGE_READWRITE
        || information.allocation_base.addr() != address
        || information.base_address.addr() > address
        || end > region_end
    {
        return Err(invalid_state(
            "canonical accounting view is not writable mapped memory",
        ));
    }
    // SAFETY: The canonical winner retains this aligned, committed writable
    // mapping until process exit. Its fixed ProcessState layout consists only
    // of AtomicU64 fields. Validate protocol identity before using the counters.
    let canonical = unsafe { &*pointer };
    if canonical.schema.load(Ordering::Acquire) != attached.schema.load(Ordering::Acquire)
        || canonical.limit_bytes.load(Ordering::Acquire)
            != attached.limit_bytes.load(Ordering::Acquire)
        || canonical.canonical_view.load(Ordering::Acquire) != address as u64
    {
        return Err(invalid_state("canonical accounting header does not match"));
    }
    Ok(canonical)
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
        // SAFETY: INVALID_HANDLE_VALUE selects pagefile-backed memory. The name
        // is NUL-terminated and remains live during the call; no attributes are
        // supplied, and bytes covers the fixed shared structure.
        let handle = unsafe {
            CreateFileMappingW(
                invalid_handle,
                std::ptr::null(),
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

    fn retain(&mut self) {
        self.view = None;
        self.handle = None;
    }

    fn close(&mut self) -> XllResult<()> {
        let mut failure = None;
        if let Some(view) = self.view {
            // SAFETY: This temporary mapping owns this view, and no reference
            // to it escapes once the distinct canonical view has been selected.
            if unsafe { UnmapViewOfFile(view.as_ptr()) } == 0 {
                failure = Some(os_error("UnmapViewOfFile"));
            } else {
                self.view = None;
            }
        }
        if let Some(handle) = self.handle {
            // SAFETY: This is the owned temporary mapping handle; closing it
            // does not unmap any view or affect the retained canonical handle.
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
    fn repeated_attachments_reuse_the_canonical_mapping() {
        let first = attach().expect("first Windows accounting attachment");
        let second = attach().expect("second Windows accounting attachment");
        assert!(std::ptr::eq(first, second));
        assert_eq!(
            first.canonical_view.load(Ordering::Acquire),
            first as *const ProcessState as usize as u64,
        );
        assert_eq!(
            first.schema.load(Ordering::Acquire),
            second.schema.load(Ordering::Acquire),
        );
        assert_eq!(
            first.limit_bytes.load(Ordering::Acquire),
            super::super::MAX_PROCESS_NUMERIC_RETURN_BYTES as u64,
        );
    }

    #[test]
    fn canonical_validation_rejects_invalid_addresses() {
        let state = attach().expect("Windows accounting attachment");
        assert!(validate_canonical(0, state).is_err());
        assert!(validate_canonical(1, state).is_err());
        assert!(validate_canonical(u64::MAX, state).is_err());
    }
}
