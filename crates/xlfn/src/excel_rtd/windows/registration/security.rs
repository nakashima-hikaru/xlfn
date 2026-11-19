//! Per-user identity and access for the cross-session registration mutex.

use super::{RTD_REGISTRATION_SCHEMA, wide_nul};
use crate::win32::{
    CloseHandle, ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, DACL_SECURITY_INFORMATION, E_INVALIDARG,
    GetCurrentProcess, GetLastError, GetSecurityInfo, GetTokenInformation, HANDLE, LocalFree,
    OWNER_SECURITY_INFORMATION, OpenProcessToken, PSECURITY_DESCRIPTOR, SDDL_REVISION_1,
    SE_KERNEL_OBJECT, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use crate::{XllError, XllResult};
use std::ffi::c_void;
use std::ptr;

const OWNER_AND_DACL: u32 = (OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION) as u32;

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: the security APIs transfer each allocation to LocalFree.
        unsafe { LocalFree(self.0) };
    }
}

/// The name and descriptor are derived together from the process's user SID.
/// Neither the logon SID nor a default token DACL is stable across sessions.
pub(super) struct MutexSecurity {
    name: String,
    descriptor: LocalAllocation,
}

impl MutexSecurity {
    pub(super) fn current_user() -> XllResult<Self> {
        let sid = current_user_sid()?;
        // MUTEX_ALL_ACCESS uses concrete rights so canonicalization does not
        // depend on the generic-rights mapping for kernel objects.
        let user_ace = if sid == "S-1-5-18" {
            String::new()
        } else {
            format!("(A;;0x001f0001;;;{sid})")
        };
        let descriptor =
            descriptor_from_sddl(&format!("O:{sid}D:P(A;;0x001f0001;;;SY){user_ace}"))?;
        Ok(Self {
            name: mutex_name(&sid),
            descriptor,
        })
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: 0,
        }
    }

    pub(super) fn validate(&self, handle: HANDLE) -> XllResult<()> {
        let mut actual = LocalAllocation(ptr::null_mut());
        // SAFETY: handle is the live owned mutex handle. The final output
        // receives a LocalFree-owned descriptor; other outputs are optional.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                OWNER_AND_DACL,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut actual.0,
            )
        };
        if status != 0 {
            return Err(XllError::WindowsApi {
                function: "GetSecurityInfo(RTD registration mutex)",
                code: status as i32,
            });
        }
        if canonical_descriptor(actual.0)? != canonical_descriptor(self.descriptor.0)? {
            return Err(invalid_security(
                "RTD registration mutex owner or DACL does not match",
            ));
        }
        Ok(())
    }
}

fn mutex_name(sid: &str) -> String {
    format!("Global\\XlFnRtdRegistration_v{RTD_REGISTRATION_SCHEMA}_{sid}")
}

fn descriptor_from_sddl(sddl: &str) -> XllResult<LocalAllocation> {
    let wide = wide_nul(sddl);
    let mut descriptor = LocalAllocation(ptr::null_mut());
    // SAFETY: wide is NUL terminated and the descriptor output is writable.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut descriptor.0,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        ));
    }
    Ok(descriptor)
}

fn canonical_descriptor(descriptor: PSECURITY_DESCRIPTOR) -> XllResult<Vec<u16>> {
    let mut output = ptr::null_mut();
    let mut length = 0;
    // SAFETY: descriptor is owned by a successful Windows security API;
    // output and length point to writable storage.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1 as u32,
            OWNER_AND_DACL,
            &mut output,
            &mut length,
        )
    } == 0
    {
        return Err(os_error(
            "ConvertSecurityDescriptorToStringSecurityDescriptorW",
        ));
    }
    let _allocation = LocalAllocation(output.cast());
    if output.is_null() || length == 0 {
        return Err(invalid_security("RTD registration mutex security is empty"));
    }
    let mut units = Vec::new();
    for index in 0..length as usize {
        // SAFETY: conversion returned length UTF-16 units including NUL.
        let unit = unsafe { output.add(index).read() };
        if unit == 0 {
            return Ok(units);
        }
        units.push(unit);
    }
    Err(invalid_security(
        "RTD registration mutex security lacks a terminator",
    ))
}

fn current_user_sid() -> XllResult<String> {
    // SAFETY: the current-process pseudo handle has no preconditions.
    let process = unsafe { GetCurrentProcess() };
    let mut token = ptr::null_mut();
    // SAFETY: process is live and token is writable handle output storage.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY as u32, &mut token) } == 0 {
        return Err(os_error("OpenProcessToken"));
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: this owner closes the process token exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
    let token = Token(token);
    let mut length = 0;
    // SAFETY: a null buffer queries the required size into writable length.
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut length) };
    if (length as usize) < size_of::<TOKEN_USER>() || length > 65_536 {
        return Err(invalid_security("invalid RTD registration token size"));
    }
    let capacity = length;
    let mut storage = vec![0_usize; (length as usize).div_ceil(size_of::<usize>())];
    // SAFETY: storage is pointer aligned and covers capacity writable bytes.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            storage.as_mut_ptr().cast(),
            capacity,
            &mut length,
        )
    } == 0
    {
        return Err(os_error("GetTokenInformation(TokenUser)"));
    }
    if length > capacity || (length as usize) < size_of::<TOKEN_USER>() {
        return Err(invalid_security("invalid RTD registration token result"));
    }
    // SAFETY: the successful API initialized an aligned TOKEN_USER and its
    // referenced SID within storage, which remains live through conversion.
    let sid = unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut output = ptr::null_mut();
    // SAFETY: sid is live and output receives a LocalFree-owned SID string.
    if unsafe { ConvertSidToStringSidW(sid, &mut output) } == 0 {
        return Err(os_error("ConvertSidToStringSidW"));
    }
    let _allocation = LocalAllocation(output.cast());
    if output.is_null() {
        return Err(invalid_security("RTD registration user SID is empty"));
    }
    let mut units = Vec::new();
    for index in 0..256 {
        // SAFETY: SID conversion returns NUL-terminated text shorter than this
        // bound; stop at its terminator without reading unused capacity.
        let unit = unsafe { output.add(index).read() };
        if unit == 0 {
            return String::from_utf16(&units)
                .map_err(|_| invalid_security("invalid RTD registration user SID"));
        }
        units.push(unit);
    }
    Err(invalid_security(
        "RTD registration user SID exceeds its bound",
    ))
}

fn os_error(function: &'static str) -> XllError {
    // SAFETY: called immediately after failure, on the same thread.
    let code = unsafe { GetLastError() } as i32;
    XllError::WindowsApi { function, code }
}

fn invalid_security(function: &'static str) -> XllError {
    XllError::WindowsApi {
        function,
        code: E_INVALIDARG,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::excel_rtd::windows::registration::{CrossProcessRegistrationGuard, MutexHandle};
    use crate::win32::CreateMutexW;

    #[test]
    fn registration_mutex_name_is_global_and_scoped_only_to_user() {
        let first = "S-1-5-21-101-202-303-1001";
        let second = "S-1-5-21-101-202-303-1002";
        assert_eq!(
            mutex_name(first),
            "Global\\XlFnRtdRegistration_v3_S-1-5-21-101-202-303-1001"
        );
        assert_ne!(mutex_name(first), mutex_name(second));
        assert_eq!(
            MutexSecurity::current_user().unwrap().name(),
            mutex_name(&current_user_sid().unwrap())
        );
    }

    #[test]
    fn registration_mutex_rejects_a_permissive_existing_object() {
        let security = MutexSecurity::current_user().unwrap();
        let name = format!(
            "{}_untrusted_{}_{}",
            security.name(),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let descriptor = descriptor_from_sddl("D:(A;;GA;;;WD)").unwrap();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let wide = wide_nul(&name);
        // SAFETY: the test retains its live descriptor and terminated name.
        let handle = unsafe { CreateMutexW(&attributes, 0, wide.as_ptr()) };
        assert!(!handle.is_null());
        let _handle = MutexHandle(handle);
        assert!(matches!(
            CrossProcessRegistrationGuard::acquire_named(&name),
            Err(XllError::WindowsApi {
                function: "RTD registration mutex owner or DACL does not match",
                code: E_INVALIDARG,
            })
        ));
    }
}
