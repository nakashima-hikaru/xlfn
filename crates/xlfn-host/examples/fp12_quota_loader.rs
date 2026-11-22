//! Launch in a fresh Windows process with two separate copies of the fixture DLL.

#[cfg(windows)]
mod native {
    use core::ffi::c_void;
    use std::ffi::{CStr, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;
    use std::ptr::{NonNull, null_mut};

    windows_link::link!("kernel32.dll" "system" fn LoadLibraryW(name: *const u16) -> *mut c_void);
    windows_link::link!("kernel32.dll" "system" fn FreeLibrary(module: *mut c_void) -> i32);
    windows_link::link!("kernel32.dll" "system" fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut *mut c_void) -> i32);
    windows_link::link!("kernel32.dll" "system" fn GetModuleHandleW(name: *const u16) -> *mut c_void);
    windows_link::link!("kernel32.dll" "system" fn GetProcAddress(module: *mut c_void, name: *const u8) -> *const c_void);
    windows_link::link!("kernel32.dll" "system" fn GetCurrentProcess() -> *mut c_void);
    windows_link::link!("kernel32.dll" "system" fn GetProcessTimes(process: *mut c_void, creation: *mut FileTime, exit: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32);
    windows_link::link!("kernel32.dll" "system" fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> *mut c_void);

    type Published = unsafe extern "C" fn() -> usize;
    type Probe = unsafe extern "C" fn(usize) -> i32;
    type NtQueryObject =
        unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32, *mut u32) -> i32;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ObjectBasicInformation {
        attributes: u32,
        granted_access: u32,
        handle_count: u32,
        pointer_count: u32,
        reserved: [u32; 10],
    }

    struct Image(Option<NonNull<c_void>>);

    impl Image {
        fn load(path: &Path) -> Self {
            assert!(path.is_absolute(), "use a full DLL path");
            let path = wide(path.as_os_str());
            // SAFETY: path is an absolute NUL-terminated path to our fixture.
            let module = unsafe { LoadLibraryW(path.as_ptr()) };
            Self(Some(NonNull::new(module).expect("LoadLibraryW fixture")))
        }

        fn handle(&self) -> NonNull<c_void> {
            self.0.expect("loaded image")
        }

        fn published_bytes(&self) -> usize {
            self.call_usize(c"fp12_fixture_published_bytes")
        }

        fn publish(&self) -> usize {
            self.call_usize(c"fp12_fixture_publish")
        }

        fn call_usize(&self, name: &CStr) -> usize {
            let address = symbol(self.handle().as_ptr(), name);
            // SAFETY: both fixture exports accepted here have this signature.
            let function: Published = unsafe { std::mem::transmute(address) };
            // SAFETY: self holds the fixture image until the export returns.
            unsafe { function() }
        }

        fn probe(&self, bytes: usize) -> i32 {
            let address = symbol(self.handle().as_ptr(), c"fp12_fixture_probe");
            // SAFETY: the named fixture export has precisely this signature.
            let function: Probe = unsafe { std::mem::transmute(address) };
            // SAFETY: self holds the fixture image until the export returns.
            unsafe { function(bytes) }
        }

        fn unload_and_check(mut self) {
            let module = self.0.take().expect("loaded image");
            let entry = symbol(module.as_ptr(), c"fp12_fixture_publish");
            assert_eq!(module_containing(entry), Some(module));
            // SAFETY: this is our one remaining owning loader reference.
            assert_ne!(unsafe { FreeLibrary(module.as_ptr()) }, 0, "FreeLibrary");
            // Query by code address only: never call or dereference it after
            // FreeLibrary. UNCHANGED_REFCOUNT avoids pinning the test image.
            assert_eq!(module_containing(entry), None, "image remains mapped");
        }
    }

    impl Drop for Image {
        fn drop(&mut self) {
            if let Some(module) = self.0.take() {
                // SAFETY: the guard still owns this loader reference.
                let _ = unsafe { FreeLibrary(module.as_ptr()) };
            }
        }
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        let wide: Vec<_> = text.encode_wide().collect();
        assert!(!wide.contains(&0));
        wide.into_iter().chain([0]).collect()
    }

    fn symbol(module: *mut c_void, name: &CStr) -> *const c_void {
        assert!(!module.is_null());
        // SAFETY: callers hold a live module and name is NUL-terminated.
        let pointer = unsafe { GetProcAddress(module, name.as_ptr().cast()) };
        assert!(!pointer.is_null(), "missing export: {name:?}");
        pointer
    }

    fn module_containing(address: *const c_void) -> Option<NonNull<c_void>> {
        const FROM_ADDRESS: u32 = 0x4;
        const UNCHANGED_REFCOUNT: u32 = 0x2;
        let mut found = null_mut();
        // SAFETY: FROM_ADDRESS interprets the pointer as a location, never
        // reads it. The writable out pointer receives a non-owning handle.
        let status = unsafe {
            GetModuleHandleExW(
                FROM_ADDRESS | UNCHANGED_REFCOUNT,
                address.cast(),
                &mut found,
            )
        };
        (status != 0).then(|| NonNull::new(found).expect("present module"))
    }

    fn section_name() -> Vec<u16> {
        let mut creation = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        // SAFETY: this call produces the current-process pseudo-handle.
        let process = unsafe { GetCurrentProcess() };
        // SAFETY: the pseudo-handle and four distinct outputs are live.
        assert_ne!(
            unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) },
            0
        );
        let creation = u64::from(creation.low) | (u64::from(creation.high) << 32);
        let pid = std::process::id();
        format!("Local\\xlfn.numeric-returns.{pid}.{creation:016x}")
            .encode_utf16()
            .chain([0])
            .collect()
    }

    fn assert_one_anchor(name: &[u16]) {
        const FILE_MAP_READ: u32 = 0x4;
        const OBJECT_BASIC_INFORMATION: u32 = 0;
        // SAFETY: name is the NUL-terminated current process' FP12 section.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) };
        assert!(!handle.is_null(), "shared section missing");
        // SAFETY: successful OpenFileMappingW transferred one owned reference.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let ntdll = wide(OsStr::new("ntdll.dll"));
        // SAFETY: ntdll is already resident in this running Windows process.
        let module = unsafe { GetModuleHandleW(ntdll.as_ptr()) };
        let address = symbol(module, c"NtQueryObject");
        // SAFETY: exact signature from the Windows ObjectBasicInformation API.
        let query: NtQueryObject = unsafe { std::mem::transmute(address) };
        let mut output = ObjectBasicInformation::default();
        let bytes = size_of::<ObjectBasicInformation>() as u32;
        // SAFETY: lease owns the object; output is a full writable structure.
        let result = unsafe {
            query(
                handle.as_raw_handle(),
                OBJECT_BASIC_INFORMATION,
                (&raw mut output).cast(),
                bytes,
                null_mut(),
            )
        };
        assert_eq!(result, 0, "NtQueryObject failed: {result:#x}");
        // This probe contributes its own temporary handle; it is closed before
        // any unload/reload, so it cannot mask loss of the production anchor.
        assert_eq!(
            output.handle_count, 2,
            "expected one anchor and the temporary query handle"
        );
    }

    pub(super) fn run() {
        let paths: Vec<_> = std::env::args_os().skip(1).collect();
        let [first, second] = paths.as_slice() else {
            panic!("pass full paths to two separate fixture DLL copies");
        };
        let name = section_name();
        let a = Image::load(Path::new(first));
        let b = Image::load(Path::new(second));
        assert_ne!(a.handle(), b.handle(), "copies were not independent images");
        let cap = xlfn_host::process_quota::MAX_PROCESS_NUMERIC_RETURN_BYTES;
        let bytes = a.publish();
        assert!(bytes > 0 && bytes < cap);
        assert_eq!(b.published_bytes(), bytes);
        assert_eq!(b.probe(cap - bytes), 1);
        assert_eq!(b.probe(cap - bytes + 1), 0);
        assert_one_anchor(&name);

        // A opened the section and won the election. Removing A proves the
        // anchor is process-owned rather than tied to its Rust static state.
        a.unload_and_check();
        assert_one_anchor(&name);
        for _ in 0..3 {
            let reloaded = Image::load(Path::new(first));
            assert_ne!(reloaded.handle(), b.handle());
            assert_eq!(reloaded.published_bytes(), bytes);
            assert_eq!(reloaded.probe(cap - bytes), 1);
            assert_eq!(reloaded.probe(cap - bytes + 1), 0);
            assert_one_anchor(&name);
            reloaded.unload_and_check();
        }
        b.unload_and_check();
        assert_one_anchor(&name);
        let late = Image::load(Path::new(second));
        assert_eq!(late.published_bytes(), bytes);
        assert_eq!(late.probe(cap - bytes + 1), 0);
        assert_one_anchor(&name);
        late.unload_and_check();
        println!(
            "FP12 section and quota survived independent DLL loads and verified physical unloads"
        );
    }
}

fn main() {
    #[cfg(windows)]
    native::run();
    #[cfg(not(windows))]
    panic!("requires native Windows");
}
