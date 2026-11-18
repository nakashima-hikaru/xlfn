//! Explicit security for the accounting section, including existing objects.

use super::{invalid_state, os_error};
use crate::XllResult;
use crate::win32::*;
use core::ffi::c_void;
use std::mem::{MaybeUninit, offset_of};
use std::ptr::NonNull;

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

struct MandatoryLabel<'descriptor> {
    ace: &'descriptor SYSTEM_MANDATORY_LABEL_ACE,
    sid: PSID,
}

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
        let mut actual = Self(LocalAllocation(std::ptr::null_mut()));
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
                &mut actual.0.0,
            )
        };
        if status != 0 {
            return Err(invalid_state("accounting mapping security is unavailable"));
        }
        // Compare owner and DACL exactly. The kernel may set SACL control
        // flags on creation, so compare the mandatory label ACE itself.
        if canonical_descriptor(actual.0.0)? != canonical_descriptor(self.0.0)? {
            return Err(invalid_state(
                "accounting mapping owner or DACL does not match",
            ));
        }
        let actual = actual.mandatory_label().ok_or_else(|| {
            invalid_state("accounting mapping has no unique mandatory integrity label")
        })?;
        let expected = self
            .mandatory_label()
            .ok_or_else(|| invalid_state("expected accounting mapping label is invalid"))?;
        // SAFETY: both SIDs were validated within their live descriptors.
        let same_sid = unsafe { EqualSid(actual.sid, expected.sid) } != 0;
        if !same_sid
            || actual.ace.Mask != expected.ace.Mask
            || actual.ace.Header.AceFlags != expected.ace.Header.AceFlags
        {
            return Err(invalid_state(
                "accounting mapping integrity label does not match",
            ));
        }
        Ok(())
    }

    fn mandatory_label(&self) -> Option<MandatoryLabel<'_>> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = MaybeUninit::<PACL>::uninit();
        // SAFETY: The descriptor is live and the outputs are writable.
        if unsafe {
            GetSecurityDescriptorSacl(self.0.0, &mut present, acl.as_mut_ptr(), &mut defaulted)
        } == 0
            || present == 0
        {
            return None;
        }
        // SAFETY: Success with a present SACL initializes the pointer output.
        let acl = NonNull::new(unsafe { acl.assume_init() })?;
        // SAFETY: A non-null SACL returned by Windows contains its ACL header.
        let acl_header = unsafe { acl.as_ref() };
        let acl_size = usize::from(acl_header.AclSize);
        if acl_header.AceCount != 1 || acl_size < size_of::<ACL>() + size_of::<ACE_HEADER>() {
            return None;
        }
        let mut ace = MaybeUninit::<*mut c_void>::uninit();
        // SAFETY: Index zero is the only entry in the live ACL.
        if unsafe { GetAce(acl.as_ptr(), 0, ace.as_mut_ptr()) } == 0 {
            return None;
        }
        // SAFETY: GetAce initialized the pointer output on success.
        let ace = NonNull::new(unsafe { ace.assume_init() })?;
        let ace_offset = ace.as_ptr().addr().checked_sub(acl.as_ptr().addr())?;
        let available = acl_size.checked_sub(ace_offset)?;
        if ace_offset < size_of::<ACL>() || available < size_of::<ACE_HEADER>() {
            return None;
        }
        // SAFETY: the ACL contains the ACE header returned by GetAce.
        let header = unsafe { ace.cast::<ACE_HEADER>().as_ref() };
        let ace_size = usize::from(header.AceSize);
        const SID_OFFSET: usize = offset_of!(SYSTEM_MANDATORY_LABEL_ACE, SidStart);
        const SID_HEADER_SIZE: usize = offset_of!(SID, SubAuthority);
        if i32::from(header.AceType) != SYSTEM_MANDATORY_LABEL_ACE_TYPE
            || ace_size > available
            || ace_size < SID_OFFSET + SID_HEADER_SIZE
        {
            return None;
        }
        // SAFETY: the validated ACE extent covers the fixed SID header.
        let sid = unsafe { ace.as_ptr().cast::<u8>().add(SID_OFFSET) };
        // SAFETY: the count byte is in the fixed header. Check it before
        // accessing the variable-length tail.
        let count = unsafe { sid.wrapping_add(offset_of!(SID, SubAuthorityCount)).read() };
        let sid_size =
            SID_HEADER_SIZE.checked_add(usize::from(count).checked_mul(size_of::<u32>())?)?;
        if sid_size > ace_size - SID_OFFSET {
            return None;
        }
        // SAFETY: the full SID extent is inside the ACE.
        if unsafe { IsValidSid(sid.cast()) } == 0 {
            return None;
        }
        // SAFETY: the validated ACE covers all fixed ACE fields. The
        // descriptor owns the memory through the returned borrow.
        let ace = unsafe { ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>().as_ref() };
        Some(MandatoryLabel {
            ace,
            sid: sid.cast(),
        })
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
