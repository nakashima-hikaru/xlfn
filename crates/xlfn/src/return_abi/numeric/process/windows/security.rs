//! Explicit security for the accounting section, including existing objects.

use super::{invalid_state, os_error};
use crate::XllResult;
use crate::win32::*;
use core::ffi::c_void;

const INFORMATION: u32 = (OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION) as u32;

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: Every non-null allocation here is returned by an API that
        // explicitly transfers a LocalFree-owned allocation to its caller.
        unsafe { LocalFree(self.0) };
    }
}

pub(super) struct Descriptor(LocalAllocation);

impl Descriptor {
    pub(super) fn private() -> XllResult<Self> {
        let sid = current_user_sid()?;
        // SECTION_ALL_ACCESS uses concrete bits so retrieval cannot expand
        // GENERIC_ALL differently from our expected descriptor.
        let user_ace = if sid == "S-1-5-18" {
            String::new()
        } else {
            format!("(A;;0x000f001f;;;{sid})")
        };
        Self::from_sddl(&format!("O:{sid}D:P(A;;0x000f001f;;;SY){user_ace}"))
    }

    fn from_sddl(sddl: &str) -> XllResult<Self> {
        let wide = sddl.encode_utf16().chain([0]).collect::<Vec<_>>();
        let mut allocation = LocalAllocation(std::ptr::null_mut());
        // SAFETY: wide is NUL terminated and the output points to live storage.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1 as u32,
                &mut allocation.0,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(os_error(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
            ));
        }
        Ok(Self(allocation))
    }

    pub(super) fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: 0,
        }
    }

    pub(super) fn validate_mapping(&self, handle: HANDLE) -> XllResult<()> {
        let mut actual = LocalAllocation(std::ptr::null_mut());
        // SAFETY: handle is an owned open section handle; only the final output
        // is requested and it receives a LocalFree-owned security descriptor.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut actual.0,
            )
        };
        if status != 0 {
            return Err(invalid_state("accounting mapping security is unavailable"));
        }
        // Serialize both descriptors with the same OS implementation. Exact
        // comparison deliberately rejects inherited, null, additional, or
        // differently privileged ACLs, including on pre-created objects.
        if canonical_descriptor(actual.0)? != canonical_descriptor(self.0.0)? {
            return Err(invalid_state(
                "accounting mapping owner or DACL does not match",
            ));
        }
        Ok(())
    }
}

fn canonical_descriptor(descriptor: PSECURITY_DESCRIPTOR) -> XllResult<Vec<u16>> {
    let mut output = std::ptr::null_mut();
    let mut length = 0;
    // SAFETY: descriptor comes from a successful Windows security API, and
    // output and length are writable local output storage.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1 as u32,
            INFORMATION,
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
        return Err(invalid_state("accounting mapping security is empty"));
    }
    let mut units = Vec::new();
    for index in 0..length as usize {
        // SAFETY: The successful conversion returns a NUL-terminated string
        // in a length-unit buffer. Stop at NUL before any spare capacity.
        let unit = unsafe { output.wrapping_add(index).read() };
        if unit == 0 {
            return Ok(units);
        }
        units.push(unit);
    }
    Err(invalid_state(
        "accounting mapping security lacks a terminator",
    ))
}

fn current_user_sid() -> XllResult<String> {
    // SAFETY: The current-process pseudo handle has no preconditions.
    let process = unsafe { GetCurrentProcess() };
    let mut token = std::ptr::null_mut();
    // SAFETY: process is valid and token is writable handle output storage.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY as u32, &mut token) } == 0 {
        return Err(os_error("OpenProcessToken"));
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: Token owns the successfully opened process-token handle.
            unsafe { CloseHandle(self.0) };
        }
    }
    let token = Token(token);
    let mut length = 0;
    // SAFETY: Null buffer is the documented size query; length is writable.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut length) };
    if (length as usize) < size_of::<TOKEN_USER>() || length > 65536 {
        return Err(invalid_state("invalid accounting token information size"));
    }
    let capacity = length;
    let mut storage = vec![0_usize; (length as usize).div_ceil(size_of::<usize>())];
    // SAFETY: storage is pointer aligned and covers capacity bytes; token is
    // live and length is independent writable output storage.
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
        return Err(os_error("GetTokenInformation"));
    }
    if length > capacity || (length as usize) < size_of::<TOKEN_USER>() {
        return Err(invalid_state("invalid accounting token information result"));
    }
    // SAFETY: The successful API initialized TOKEN_USER in aligned storage.
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let mut output = std::ptr::null_mut();
    // SAFETY: user.User.Sid is the live SID returned within the token buffer;
    // output receives a LocalFree-owned NUL-terminated SID string.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut output) } == 0 {
        return Err(os_error("ConvertSidToStringSidW"));
    }
    let _allocation = LocalAllocation(output.cast());
    if output.is_null() {
        return Err(invalid_state("user SID text is unavailable"));
    }
    let mut units = Vec::new();
    for index in 0..256 {
        // SAFETY: Successful SID conversion returns a NUL-terminated string;
        // standard SID text is shorter than this bound. Stop at its terminator.
        let unit = unsafe { output.wrapping_add(index).read() };
        if unit == 0 {
            return String::from_utf16(&units).map_err(|_| invalid_state("invalid user SID text"));
        }
        units.push(unit);
    }
    Err(invalid_state("user SID text exceeds its bound"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_precreated_mapping_is_rejected() {
        let name = format!(
            "Local\\xlfn.numeric-untrusted.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
        let permissive = Descriptor::from_sddl("D:(A;;GA;;;WD)").unwrap();
        let attributes = permissive.attributes();
        // SAFETY: A pagefile mapping is created with a live descriptor and name.
        let handle = unsafe {
            super::super::CreateFileMappingW(
                std::ptr::without_provenance_mut(usize::MAX),
                (&raw const attributes).cast(),
                super::super::PAGE_READWRITE,
                0,
                4096,
                name.as_ptr(),
            )
        };
        assert!(!handle.is_null());
        assert!(super::super::Mapping::open(&name).is_err());
        // SAFETY: This test owns the handle and has not created any views.
        unsafe { CloseHandle(handle) };
    }
}
