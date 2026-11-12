//! Explicit security for the accounting section, including existing objects.

use super::{invalid_state, os_error};
use crate::XllResult;
use crate::win32::*;
use core::ffi::c_void;

const OWNER_AND_DACL: u32 = (OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION) as u32;

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
        let sid = token_sid(TokenUser)?;
        let integrity = token_sid(TokenIntegrityLevel)?;
        // SECTION_ALL_ACCESS uses concrete bits so retrieval cannot expand
        // GENERIC_ALL differently from our expected descriptor.
        let user_ace = if sid == "S-1-5-18" {
            String::new()
        } else {
            format!("(A;;0x000f001f;;;{sid})")
        };
        Self::from_sddl(&format!(
            "O:{sid}D:P(A;;0x000f001f;;;SY){user_ace}S:(ML;;NWNR;;;{integrity})"
        ))
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
                OWNER_AND_DACL | LABEL_SECURITY_INFORMATION as u32,
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
        let actual = canonical_descriptor(actual.0)?;
        let expected = canonical_descriptor(self.0.0)?;
        if actual != expected {
            #[cfg(test)]
            {
                let actual = String::from_utf16_lossy(&actual);
                let expected = String::from_utf16_lossy(&expected);
                let actual_label = actual.split_once("S:").map(|(_, label)| label);
                let expected_label = expected.split_once("S:").map(|(_, label)| label);
                eprintln!(
                    "mapping integrity label: actual={actual_label:?}, expected={expected_label:?}"
                );
            }
            return Err(invalid_state(
                "accounting mapping owner, DACL or integrity label does not match",
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
            // The queried label is stored as an ACE in the returned SACL.
            OWNER_AND_DACL | SACL_SECURITY_INFORMATION as u32,
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

fn token_sid(class: TOKEN_INFORMATION_CLASS) -> XllResult<String> {
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
    unsafe { GetTokenInformation(token.0, class, std::ptr::null_mut(), 0, &mut length) };
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
            class,
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
    let sid = if class == TokenUser {
        // SAFETY: TokenUser initialized this aligned TOKEN_USER buffer.
        unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    } else {
        // SAFETY: The only other caller requests TokenIntegrityLevel, whose
        // leading SID_AND_ATTRIBUTES has the same size and alignment.
        unsafe {
            (*storage.as_ptr().cast::<TOKEN_MANDATORY_LABEL>())
                .Label
                .Sid
        }
    };
    let mut output = std::ptr::null_mut();
    // SAFETY: user.User.Sid is the live SID returned within the token buffer;
    // output receives a LocalFree-owned NUL-terminated SID string.
    if unsafe { ConvertSidToStringSidW(sid, &mut output) } == 0 {
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
    fn same_owner_mapping_without_integrity_label_is_rejected() {
        let name = format!(
            "Local\\xlfn.label-test.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
        .encode_utf16()
        .chain([0])
        .collect::<Vec<_>>();
        let sid = token_sid(TokenUser).unwrap();
        let descriptor = Descriptor::from_sddl(&format!(
            "O:{sid}D:P(A;;0x000f001f;;;SY)(A;;0x000f001f;;;{sid})S:(ML;;NW;;;LW)"
        ))
        .unwrap();
        let attributes = descriptor.attributes();
        // SAFETY: the descriptor and name remain live throughout creation.
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
        let rejected = super::super::Mapping::open(&name).is_err();
        // SAFETY: the test owns this successfully created handle.
        unsafe { CloseHandle(handle) };
        assert!(rejected);
    }

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
