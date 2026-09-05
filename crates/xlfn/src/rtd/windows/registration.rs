use super::ActiveServer;
use crate::win32::{
    CloseHandle, CreateMutexW, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, GUID,
    GetLastError, HANDLE, HKEY, HKEY_CURRENT_USER, INFINITE, KEY_READ, KEY_WRITE,
    REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegEnumKeyExW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, ReleaseMutex, WAIT_ABANDONED, WAIT_FAILED,
    WAIT_OBJECT_0, WaitForSingleObject,
};
use crate::{XllError, XllResult};
use parking_lot::{Mutex, MutexGuard};
use std::ptr;

// Schema 2 registrations are protected by one cross-process mutex for the
// complete temporary registration lifetime. Do not scavenge schema 1 entries:
// an older live XLL does not participate in that protocol, so its registration
// cannot be proven stale.
pub(super) const RTD_REGISTRATION_OWNER: &str = "xlfn";
pub(super) const RTD_REGISTRATION_SCHEMA: &str = "2";
pub(super) const RTD_PROG_ID_PREFIX: &str = "XlFnRtd_";
pub(super) static REGISTRATION_MAINTENANCE: Mutex<()> = Mutex::new(());

pub(super) struct TemporaryRegistration {
    prog_key: Vec<u16>,
    class_key: Vec<u16>,
    _maintenance: MutexGuard<'static, ()>,
    _cross_process: CrossProcessRegistrationGuard,
}

impl TemporaryRegistration {
    pub(super) fn new(active: &ActiveServer, module_path: &str) -> XllResult<Self> {
        let maintenance = REGISTRATION_MAINTENANCE.lock();
        let cross_process = CrossProcessRegistrationGuard::acquire()?;

        if let Err(error) = scavenge_owned_registrations(module_path, Some(&active.prog_id)) {
            crate::diagnostics::report_no_unwind("RTD registration scavenging", &error);
        }

        let class = guid_braced(active.class_id);
        let prog_key = format!("Software\\Classes\\{}", active.prog_id);
        let class_key = format!("Software\\Classes\\CLSID\\{class}");
        let registration = Self {
            prog_key: wide_nul(&prog_key),
            class_key: wide_nul(&class_key),
            _maintenance: maintenance,
            _cross_process: cross_process,
        };

        let result = (|| {
            set_registry_value(&prog_key, Some("XlFnOwner"), RTD_REGISTRATION_OWNER)?;
            set_registry_value(
                &prog_key,
                Some("XlFnRegistrationSchema"),
                RTD_REGISTRATION_SCHEMA,
            )?;
            set_registry_value(&prog_key, Some("XlFnOwnerModule"), module_path)?;
            set_registry_value(&prog_key, Some("XlFnClassId"), &class)?;
            set_registry_value(&format!("{prog_key}\\CLSID"), None, &class)?;
            set_registry_value(&format!("{class_key}\\InProcServer32"), None, module_path)?;
            set_registry_value(
                &format!("{class_key}\\InProcServer32"),
                Some("ThreadingModel"),
                "Both",
            )?;
            set_registry_value(&format!("{class_key}\\ProgID"), None, &active.prog_id)?;
            Ok(())
        })();

        if let Err(error) = result {
            drop(registration);
            return Err(error);
        }

        Ok(registration)
    }

    fn close_internal(&mut self) -> XllResult<()> {
        let mut first_error = None;
        // SAFETY: both paths are valid NUL-terminated HKCU subkeys created and
        // owned by this guard.
        unsafe {
            let class_status = RegDeleteTreeW(HKEY_CURRENT_USER, self.class_key.as_ptr());
            if !matches!(class_status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
                first_error = Some(registry_error("RegDeleteTreeW (CLSID)", class_status));
                record_registry_key_debt(String::from_utf16_lossy(&self.class_key));
            }

            let prog_status = RegDeleteTreeW(HKEY_CURRENT_USER, self.prog_key.as_ptr());
            if !matches!(prog_status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
                if first_error.is_none() {
                    first_error = Some(registry_error("RegDeleteTreeW (ProgID)", prog_status));
                }
                record_registry_key_debt(String::from_utf16_lossy(&self.prog_key));
            }
        }

        if let Some(err) = first_error {
            Err(err)
        } else {
            Ok(())
        }
    }
}

static REGISTRY_KEY_DEBT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn record_registry_key_debt(key_path: String) {
    let mut debt = REGISTRY_KEY_DEBT.lock();
    if !debt.contains(&key_path) {
        debt.push(key_path);
    }
}

impl Drop for TemporaryRegistration {
    fn drop(&mut self) {
        let _ = self.close_internal();
    }
}

pub(super) struct CrossProcessRegistrationGuard {
    handle: HANDLE,
}

impl CrossProcessRegistrationGuard {
    fn acquire() -> XllResult<Self> {
        // The registry location is shared by all xlfn modules for one user, so
        // use one session-wide acquisition order rather than deriving a name
        // from a path string that may have aliases (case, short names, or symlinks).
        Self::acquire_named("Local\\XlFnRtdRegistration_v1")
    }

    pub(super) fn acquire_named(name: &str) -> XllResult<Self> {
        let name = wide_nul(name);

        // SAFETY: the security attributes are optional, initial ownership is
        // false, and `name` is a live NUL-terminated UTF-16 buffer.
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            // SAFETY: GetLastError has no preconditions and is read immediately
            // after the failed CreateMutexW call.
            let code = (unsafe { GetLastError() }) as i32;
            return Err(XllError::WindowsApi {
                function: "CreateMutexW",
                code,
            });
        }

        // SAFETY: `handle` is a live mutex handle returned by CreateMutexW.
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait != (WAIT_OBJECT_0 as u32) && wait != (WAIT_ABANDONED as u32) {
            // SAFETY: `handle` is owned by this function and has not been closed.
            unsafe { CloseHandle(handle) };
            return Err(XllError::WindowsApi {
                function: "WaitForSingleObject(RTD registration mutex)",
                code: if wait == WAIT_FAILED {
                    // SAFETY: GetLastError is read immediately after WAIT_FAILED.
                    (unsafe { GetLastError() }) as i32
                } else {
                    wait as i32
                },
            });
        }

        Ok(Self { handle })
    }
}

impl Drop for CrossProcessRegistrationGuard {
    fn drop(&mut self) {
        // SAFETY: this guard owns the mutex after a successful wait and closes
        // the handle exactly once after releasing ownership.
        unsafe {
            ReleaseMutex(self.handle);
            CloseHandle(self.handle);
        }
    }
}

pub(super) fn scavenge_owned_registrations(
    module_path: &str,
    keep_prog_id: Option<&str>,
) -> XllResult<()> {
    for prog_id in rtd_prog_ids()? {
        if keep_prog_id.is_some_and(|keep| prog_id.eq_ignore_ascii_case(keep)) {
            continue;
        }

        let prog_key = format!("Software\\Classes\\{prog_id}");
        let owner = read_registry_string(&prog_key, "XlFnOwner")?;
        let schema = read_registry_string(&prog_key, "XlFnRegistrationSchema")?;
        let owner_module = read_registry_string(&prog_key, "XlFnOwnerModule")?;
        let class_id = read_registry_string(&prog_key, "XlFnClassId")?;

        if owner.as_deref() != Some(RTD_REGISTRATION_OWNER)
            || schema.as_deref() != Some(RTD_REGISTRATION_SCHEMA)
            || !owner_module
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(module_path))
        {
            continue;
        }

        let Some(class_id) = class_id.filter(|value| is_braced_guid(value)) else {
            continue;
        };

        // Delete the CLSID first. If it fails, the marked ProgID remains so a
        // later startup can retry instead of orphaning an undiscoverable CLSID.
        delete_registry_tree(&format!("Software\\Classes\\CLSID\\{class_id}"))?;
        delete_registry_tree(&prog_key)?;
    }

    Ok(())
}

fn delete_registry_tree(path: &str) -> XllResult<()> {
    let path = wide_nul(path);

    // SAFETY: `path` is a valid NUL-terminated HKCU-relative registry subkey.
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) };

    if matches!(status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(registry_error("RegDeleteTreeW", status))
    }
}

fn rtd_prog_ids() -> XllResult<Vec<String>> {
    let classes_path = wide_nul("Software\\Classes");
    let mut classes: HKEY = ptr::null_mut();

    // SAFETY: `classes_path` is NUL-terminated and `classes` points to writable
    // HKEY output storage.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            classes_path.as_ptr(),
            0,
            KEY_READ as u32,
            &mut classes,
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(Vec::new());
    }

    if status != ERROR_SUCCESS {
        return Err(registry_error("RegOpenKeyExW", status));
    }

    let mut names = Vec::new();
    let mut index = 0;

    loop {
        // Windows registry key names are limited to 255 UTF-16 code units.
        let mut name = [0_u16; 256];
        let mut length = name.len() as u32;

        // SAFETY: `classes` is an open registry key, `name` is writable for
        // `length` UTF-16 units, and all unused optional outputs are null.
        let status = unsafe {
            RegEnumKeyExW(
                classes,
                index,
                name.as_mut_ptr(),
                &mut length,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };

        if status == ERROR_NO_MORE_ITEMS {
            break;
        }

        if status != ERROR_SUCCESS {
            // SAFETY: `classes` was successfully returned by RegOpenKeyExW and
            // has not previously been closed.
            unsafe { RegCloseKey(classes) };
            return Err(registry_error("RegEnumKeyExW", status));
        }

        let value = String::from_utf16_lossy(&name[..length as usize]);
        if value.starts_with(RTD_PROG_ID_PREFIX) {
            names.push(value);
        }

        index += 1;
    }

    // SAFETY: `classes` was successfully returned by RegOpenKeyExW and is
    // closed exactly once after enumeration completes.
    unsafe { RegCloseKey(classes) };

    Ok(names)
}

pub(super) fn read_registry_string(path: &str, name: &str) -> XllResult<Option<String>> {
    let path = wide_nul(path);
    let name = wide_nul(name);
    let mut key: HKEY = ptr::null_mut();

    // SAFETY: `path` is NUL-terminated and `key` points to writable HKEY output
    // storage.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            KEY_READ as u32,
            &mut key,
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }

    if status != ERROR_SUCCESS {
        return Err(registry_error("RegOpenKeyExW", status));
    }

    let mut value_type = 0;
    let mut bytes = 0;

    // SAFETY: `key` is open, `name` is NUL-terminated, and both metadata
    // outputs are writable. A null data pointer requests the required size.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            &mut value_type,
            ptr::null_mut(),
            &mut bytes,
        )
    };

    if status == ERROR_FILE_NOT_FOUND {
        // SAFETY: `key` was returned by RegOpenKeyExW and has not yet been closed.
        unsafe { RegCloseKey(key) };
        return Ok(None);
    }

    if status != ERROR_SUCCESS || value_type != REG_SZ {
        // SAFETY: `key` was returned by RegOpenKeyExW and has not yet been closed.
        unsafe { RegCloseKey(key) };

        return if status == ERROR_SUCCESS {
            Ok(None)
        } else {
            Err(registry_error("RegQueryValueExW", status))
        };
    }

    let mut value = vec![0_u16; (bytes as usize).div_ceil(2)];

    // SAFETY: `value` provides at least `bytes` writable bytes, `key` remains
    // open, and all metadata pointers are valid.
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            &mut value_type,
            value.as_mut_ptr().cast(),
            &mut bytes,
        )
    };

    // SAFETY: `key` was returned by RegOpenKeyExW and is closed exactly once.
    unsafe { RegCloseKey(key) };

    if status != ERROR_SUCCESS {
        return Err(registry_error("RegQueryValueExW", status));
    }

    let length = (bytes as usize / 2).min(value.len());
    value.truncate(length);

    while value.last() == Some(&0) {
        value.pop();
    }

    Ok(Some(String::from_utf16_lossy(&value)))
}

fn registry_error(function: &'static str, status: i32) -> XllError {
    XllError::WindowsApi {
        function,
        code: status,
    }
}

fn is_braced_guid(value: &str) -> bool {
    value.len() == 38
        && value.as_bytes()[0] == b'{'
        && value.as_bytes()[37] == b'}'
        && [9, 14, 19, 24]
            .into_iter()
            .all(|index| value.as_bytes()[index] == b'-')
        && value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 0 | 9 | 14 | 19 | 24 | 37) || byte.is_ascii_hexdigit()
        })
}

pub(super) fn set_registry_value(path: &str, name: Option<&str>, value: &str) -> XllResult<()> {
    let path = wide_nul(path);
    let name = name.map(wide_nul);
    let value = wide_nul(value);
    let mut key: HKEY = ptr::null_mut();
    let mut disposition = 0;

    // SAFETY: all input string buffers are NUL-terminated and remain readable.
    // `key` and `disposition` point to writable output storage.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            path.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE as u32,
            KEY_WRITE as u32,
            ptr::null(),
            &mut key,
            &mut disposition,
        )
    };

    if status != ERROR_SUCCESS {
        return Err(XllError::WindowsApi {
            function: "RegCreateKeyExW",
            code: status,
        });
    }

    let bytes = u32::try_from(value.len() * 2).map_err(|_| XllError::Domain {
        code: crate::error::DomainErrorCode::Overflow,
    })?;

    // SAFETY: `key` is open, the optional name and value buffers are
    // NUL-terminated and readable, and `bytes` does not exceed the value buffer.
    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
            0,
            REG_SZ,
            value.as_ptr().cast(),
            bytes,
        )
    };

    // SAFETY: `key` was returned by RegCreateKeyExW and is closed exactly once.
    unsafe { RegCloseKey(key) };

    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(XllError::WindowsApi {
            function: "RegSetValueExW",
            code: status as i32,
        })
    }
}

pub(super) fn wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

pub(super) fn guid_compact(guid: GUID) -> String {
    guid_braced(guid).replace(['{', '}', '-'], "")
}

pub(super) fn guid_braced(guid: GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7],
    )
}
