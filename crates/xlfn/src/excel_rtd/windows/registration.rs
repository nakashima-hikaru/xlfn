use super::ActiveServer;
use crate::sync::{Mutex, MutexGuard};
use crate::win32::{
    CloseHandle, CreateMutexW, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, GUID,
    GetLastError, HANDLE, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegEnumKeyExW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW, ReleaseMutex, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0,
    WaitForSingleObject,
};
use crate::{XllError, XllResult};
use std::ptr;

// Schema 2 registrations are protected by one cross-process mutex for the
// complete temporary registration lifetime. Do not scavenge schema 1 entries:
// an older live XLL does not participate in that protocol, so its registration
// cannot be proven stale.
pub(super) const RTD_REGISTRATION_OWNER: &str = "xlfn";
pub(super) const RTD_REGISTRATION_SCHEMA: &str = "2";
pub(super) const RTD_PROG_ID_PREFIX: &str = "XlFnRtd_";
pub(super) static REGISTRATION_MAINTENANCE: Mutex<()> = Mutex::new(());
const REGISTRATION_MUTEX_TIMEOUT_MS: u32 = 1_000;
const CLASSES_KEY: &str = "Software\\Classes";
const CLSID_KEY: &str = "Software\\Classes\\CLSID";
// WOW64 rejects volatile creation under the redirected CLSID key. Keep both
// keys persistent on x86 so scavenging can still find the CLSID after a reboot.
#[cfg(target_arch = "x86")]
const REGISTRATION_KEY_OPTIONS: u32 = crate::win32::REG_OPTION_NON_VOLATILE as u32;
#[cfg(not(target_arch = "x86"))]
const REGISTRATION_KEY_OPTIONS: u32 = crate::win32::REG_OPTION_VOLATILE as u32;

pub(super) struct TemporaryRegistration {
    prog_key: Vec<u16>,
    class_key: Vec<u16>,
    _maintenance: MutexGuard<'static, ()>,
    _cross_process: CrossProcessRegistrationGuard,
}

impl TemporaryRegistration {
    pub(super) fn new(active: &ActiveServer, module_path: &str) -> XllResult<Self> {
        let identity = &active.identity;
        let maintenance = REGISTRATION_MAINTENANCE.lock();
        let cross_process = CrossProcessRegistrationGuard::acquire()?;

        if let Err(error) = scavenge_owned_registrations(module_path, Some(identity.prog_id())) {
            crate::diagnostics::report_no_unwind("RTD registration scavenging", &error);
        }

        let class = guid_braced(*identity.class_id());
        // RegCreateKeyExW applies its options to every missing component.
        // Open the shared ancestors as persistent keys before adding the
        // temporary children, in the same default registry view as cleanup.
        let classes = RegistryKey::persistent_parent(CLASSES_KEY)?;
        let clsid = RegistryKey::persistent_parent(CLSID_KEY)?;
        let registration = Self {
            prog_key: wide_nul(&format!("{CLASSES_KEY}\\{}", identity.prog_id())),
            class_key: wide_nul(&format!("{CLSID_KEY}\\{class}")),
            _maintenance: maintenance,
            _cross_process: cross_process,
        };

        let result = (|| {
            let prog_key = classes.temporary_child(identity.prog_id())?;
            prog_key.set_value(Some("XlFnOwner"), RTD_REGISTRATION_OWNER)?;
            prog_key.set_value(Some("XlFnRegistrationSchema"), RTD_REGISTRATION_SCHEMA)?;
            prog_key.set_value(Some("XlFnOwnerModule"), module_path)?;
            prog_key.set_value(Some("XlFnClassId"), &class)?;
            prog_key.temporary_child("CLSID")?.set_value(None, &class)?;
            let class_key = clsid.temporary_child(&class)?;
            let in_proc = class_key.temporary_child("InProcServer32")?;
            in_proc.set_value(None, module_path)?;
            in_proc.set_value(Some("ThreadingModel"), "Both")?;
            class_key
                .temporary_child("ProgID")?
                .set_value(None, identity.prog_id())?;
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
        Self::acquire_named_with_timeout(name, REGISTRATION_MUTEX_TIMEOUT_MS)
    }

    pub(super) fn acquire_named_with_timeout(name: &str, timeout_ms: u32) -> XllResult<Self> {
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
        let wait = unsafe { WaitForSingleObject(handle, timeout_ms) };
        if wait != (WAIT_OBJECT_0 as u32) && wait != (WAIT_ABANDONED as u32) {
            let code = if wait == WAIT_FAILED {
                // SAFETY: GetLastError is read immediately after WAIT_FAILED.
                (unsafe { GetLastError() }) as i32
            } else {
                wait as i32
            };
            // SAFETY: this function owns the handle but acquired no mutex ownership.
            unsafe { CloseHandle(handle) };
            return Err(XllError::WindowsApi {
                function: "WaitForSingleObject(RTD registration mutex)",
                code,
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
    let mut first_error = None;
    for prog_id in rtd_prog_ids()? {
        if keep_prog_id.is_some_and(|keep| prog_id.eq_ignore_ascii_case(keep)) {
            continue;
        }

        let result = (|| -> XllResult<()> {
            let prog_key = format!("Software\\Classes\\{prog_id}");
            let owner = read_registry_string(&prog_key, "XlFnOwner")?;
            let schema = read_registry_string(&prog_key, "XlFnRegistrationSchema")?;
            let owner_module = read_registry_string(&prog_key, "XlFnOwnerModule")?;
            let class_id = read_registry_string(&prog_key, "XlFnClassId")?;

            if owner.as_deref() != Some(RTD_REGISTRATION_OWNER)
                || schema.as_deref() != Some(RTD_REGISTRATION_SCHEMA)
                || !owner_module.as_deref().is_some_and(|owner| {
                    owner.eq_ignore_ascii_case(module_path)
                        || std::path::Path::new(owner)
                            .try_exists()
                            .is_ok_and(|exists| !exists)
                })
            {
                return Ok(());
            }

            let expected_class = class_id_from_prog_id(&prog_id);
            let Some(class_id) = class_id.filter(|value| {
                expected_class
                    .as_deref()
                    .is_some_and(|expected| value.eq_ignore_ascii_case(expected))
            }) else {
                report_unowned_registration();
                return Ok(());
            };
            let class_key = format!("Software\\Classes\\CLSID\\{class_id}");
            let prog_class = read_registry_string(&format!("{prog_key}\\CLSID"), "")?;
            let class_prog = read_registry_string(&format!("{class_key}\\ProgID"), "")?;
            let class_module = read_registry_string(&format!("{class_key}\\InProcServer32"), "")?;
            if !prog_class
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(&class_id))
                || !class_prog
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(&prog_id))
                || !class_module.as_deref().is_some_and(|value| {
                    owner_module
                        .as_deref()
                        .is_some_and(|owner| value.eq_ignore_ascii_case(owner))
                })
            {
                report_unowned_registration();
                return Ok(());
            }

            // Delete the CLSID first. If it fails, the marked ProgID remains so a
            // later startup can retry instead of orphaning an undiscoverable CLSID.
            delete_registry_tree(&format!("Software\\Classes\\CLSID\\{class_id}"))?;
            delete_registry_tree(&prog_key)?;
            Ok(())
        })();
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }

    first_error.map_or(Ok(()), Err)
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

/// Filter the registry's wide key names before allocating UTF-8 strings.
fn is_rtd_prog_id_utf16(name: &[u16]) -> bool {
    let prefix = RTD_PROG_ID_PREFIX.as_bytes();
    name.len() >= prefix.len()
        && name[..prefix.len()]
            .iter()
            .zip(prefix)
            .all(|(&unit, &byte)| unit == u16::from(byte))
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

        let units = &name[..length as usize];
        if is_rtd_prog_id_utf16(units) {
            names.push(String::from_utf16_lossy(units));
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

fn report_unowned_registration() {
    crate::diagnostics::report_no_unwind(
        "RTD registration ownership mismatch; preserving registration",
        &XllError::WindowsApi {
            function: "RTD registration ownership validation",
            code: crate::win32::E_INVALIDARG,
        },
    );
}

fn class_id_from_prog_id(prog_id: &str) -> Option<String> {
    let compact = prog_id.strip_prefix(RTD_PROG_ID_PREFIX)?;
    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!(
        "{{{}-{}-{}-{}-{}}}",
        &compact[..8],
        &compact[8..12],
        &compact[12..16],
        &compact[16..20],
        &compact[20..]
    ))
}

struct RegistryKey(HKEY);

impl RegistryKey {
    fn persistent_parent(path: &str) -> XllResult<Self> {
        Self::create(HKEY_CURRENT_USER, path, REG_OPTION_NON_VOLATILE as u32)
    }

    fn create(parent: HKEY, path: &str, options: u32) -> XllResult<Self> {
        let path = wide_nul(path);
        let mut key = ptr::null_mut();
        // SAFETY: the parent is a live registry key, the path is NUL
        // terminated, and key points to writable storage.
        let status = unsafe {
            RegCreateKeyExW(
                parent,
                path.as_ptr(),
                0,
                ptr::null(),
                options,
                KEY_WRITE as u32,
                ptr::null(),
                &mut key,
                ptr::null_mut(),
            )
        };
        if status == ERROR_SUCCESS {
            Ok(Self(key))
        } else {
            Err(registry_error("RegCreateKeyExW", status))
        }
    }

    fn temporary_child(&self, path: &str) -> XllResult<Self> {
        Self::create(self.0, path, REGISTRATION_KEY_OPTIONS)
    }

    fn set_value(&self, name: Option<&str>, value: &str) -> XllResult<()> {
        let name = name.map(wide_nul);
        let value = wide_nul(value);
        let bytes = u32::try_from(size_of_val(value.as_slice())).map_err(|_| XllError::Domain {
            code: crate::error::DomainErrorCode::Overflow,
        })?;

        // SAFETY: this key is open and both buffers are NUL terminated.
        let status = unsafe {
            RegSetValueExW(
                self.0,
                name.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                bytes,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(registry_error("RegSetValueExW", status as i32))
        }
    }
}

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: this key owns the handle returned by RegCreateKeyExW.
        unsafe { RegCloseKey(self.0) };
    }
}

#[cfg(test)]
pub(super) fn set_registry_value(path: &str, name: Option<&str>, value: &str) -> XllResult<()> {
    let (parent, child) = if let Some(child) = path.strip_prefix("Software\\Classes\\CLSID\\") {
        (CLSID_KEY, child)
    } else {
        (
            CLASSES_KEY,
            path.strip_prefix("Software\\Classes\\").unwrap(),
        )
    };
    RegistryKey::persistent_parent(parent)?
        .temporary_child(child)?
        .set_value(name, value)
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

#[cfg(test)]
mod registry_prefix_tests {
    use super::*;

    #[test]
    fn new_shared_ancestors_accept_persistent_children_after_temporary_registration() {
        let root = format!(
            "Software\\xlfn-rtd-registry-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let _cleanup = std::mem::DropGuard::new(wide_nul(&root), |root| {
            // SAFETY: this private test key has no other owner and its path is NUL terminated.
            unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, root.as_ptr()) };
        });
        let classes = RegistryKey::persistent_parent(&format!("{root}\\Classes")).unwrap();
        let clsid = RegistryKey::persistent_parent(&format!("{root}\\Classes\\CLSID")).unwrap();
        let _prog_key = classes.temporary_child("TemporaryProgID").unwrap();
        let _class_key = clsid.temporary_child("TemporaryClassID").unwrap();
        RegistryKey::create(clsid.0, "PersistentClassID", REG_OPTION_NON_VOLATILE as u32).unwrap();
    }

    #[test]
    fn filter_registry_keys_before_utf16_decoding() {
        let matching = "XlFnRtd_00112233445566778899AABBCCDDEEFF"
            .encode_utf16()
            .collect::<Vec<_>>();
        let unrelated = "OtherComponent_価格".encode_utf16().collect::<Vec<_>>();
        let upper = "XLFNRTD_00112233445566778899AABBCCDDEEFF"
            .encode_utf16()
            .collect::<Vec<_>>();
        assert!(is_rtd_prog_id_utf16(&matching));
        assert!(!is_rtd_prog_id_utf16(&matching[..7]));
        assert!(!is_rtd_prog_id_utf16(&unrelated));
        // Preserve the previous starts_with case-sensitive matching policy.
        assert!(!is_rtd_prog_id_utf16(&upper));

        let mut malformed_suffix = "XlFnRtd_".encode_utf16().collect::<Vec<_>>();
        malformed_suffix.push(0xD800);
        assert!(is_rtd_prog_id_utf16(&malformed_suffix));
    }
}
