use super::*;

/// A directory that was created or adopted after its private staging
/// invariants were established.  Packaging APIs accept this capability
/// instead of an arbitrary path so callers cannot accidentally stage into a
/// directory that was public during construction.
#[derive(Debug)]
pub struct PrivateStagingDirectory {
    pub(crate) path: PathBuf,
    pub(crate) identity: DirectoryIdentity,
    pub(crate) handle: std::fs::File,
}

impl PrivateStagingDirectory {
    /// Creates a new private directory with its private attributes applied as
    /// part of the create operation itself.
    pub fn create(path: &Path) -> PackageResult<Self> {
        if fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "private staging directory already exists: {}",
                path.display()
            )
            .into());
        }

        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        validate_directory_path(parent)?;
        fs::create_dir_all(parent)?;
        validate_directory_path(parent)?;
        create_private_directory(path)?;
        Self::open(path)
    }

    /// Adopts an already-created directory after verifying its identity and
    /// private attributes.  This is intended for directories created by an
    /// OS-backed temporary-directory primitive.
    pub fn open(path: &Path) -> PackageResult<Self> {
        validate_path_components(path)?;
        let handle = open_private_directory(path).map_err(|error| {
            PackageError::Message(format!(
                "failed to open private staging directory {}: {error}",
                path.display()
            ))
        })?;
        let metadata = handle.metadata()?;
        validate_private_directory(path, &metadata)?;
        let identity = DirectoryIdentity(file_snapshot_state(&handle)?.identity);
        Ok(Self {
            path: path.to_path_buf(),
            identity,
            handle,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn verify(&self) -> PackageResult {
        if validate_path_components(&self.path).is_err() {
            return Err(PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            });
        }
        let path_handle = open_private_directory(&self.path).map_err(|_| {
            PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            }
        })?;
        let path_metadata =
            path_handle
                .metadata()
                .map_err(|_| PackageError::StagingDirectoryReplaced {
                    path: self.path.clone(),
                })?;
        if validate_private_directory(&self.path, &path_metadata).is_err()
            || DirectoryIdentity(file_snapshot_state(&path_handle)?.identity) != self.identity
        {
            return Err(PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            });
        }

        let handle_metadata = self.handle.metadata()?;
        validate_private_directory(&self.path, &handle_metadata)?;
        let handle_identity = DirectoryIdentity(file_snapshot_state(&self.handle)?.identity);
        if handle_identity != self.identity {
            return Err(PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}
pub(crate) fn path_is_within(root: &Path, candidate: &Path) -> bool {
    let mut candidate_components = candidate.components();
    root.components().all(|root_component| {
        candidate_components
            .next()
            .is_some_and(|candidate_component| component_eq(root_component, candidate_component))
    })
}

pub(crate) trait SnapshotObserver {
    fn after_open(&self, _path: &Path) {}

    fn after_first_chunk(&self, _path: &Path) {}
}

pub(crate) struct NoopSnapshotObserver;

impl SnapshotObserver for NoopSnapshotObserver {}

pub(crate) fn open_bundle_source_for_snapshot(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    #[cfg(target_os = "windows")]
    {
        use crate::win32::FILE_SHARE_READ;
        use std::os::windows::fs::OpenOptionsExt;

        // Other readers remain allowed, but writers and delete/rename operations
        // are rejected while this handle is alive.
        options.share_mode(FILE_SHARE_READ);
    }

    options.open(path)
}

pub(crate) fn open_staged_file_no_follow(path: &Path) -> io::Result<std::fs::File> {
    open_staged_path_no_follow_with_kind(path, false)
}

pub(crate) fn open_commit_source_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }

    #[cfg(target_os = "windows")]
    {
        use crate::win32::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ};
        use std::os::windows::fs::OpenOptionsExt;

        // Refuse new writers while the final stable-content check runs.
        // FILE_SHARE_DELETE permits file-level delete/rename access, but
        // Windows still requires these descendant handles to be closed before
        // their non-empty parent directory can be renamed.
        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    }

    options.open(path)
}

pub(crate) fn open_staged_directory_no_follow(path: &Path) -> io::Result<std::fs::File> {
    open_staged_path_no_follow_with_kind(path, true)
}

pub(crate) fn open_staged_path_no_follow_with_kind(
    path: &Path,
    directory: bool,
) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    #[cfg(not(target_os = "windows"))]
    let _ = directory;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // O_NOFOLLOW makes the final component an object identity check rather
        // than a path lookup. The staging parent is private and the directory
        // identity is checked again after all entries have been opened.
        options.custom_flags(libc::O_NOFOLLOW);
    }

    #[cfg(target_os = "windows")]
    {
        use crate::win32::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        use std::os::windows::fs::OpenOptionsExt;

        let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
        if directory {
            flags |= FILE_FLAG_BACKUP_SEMANTICS;
        }
        let share_mode = if directory {
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        } else {
            FILE_SHARE_READ
        };
        options
            .custom_flags(flags)
            // Keep writers and deletion from racing the read. These handles
            // are closed before the directory rename. The long-lived
            // PrivateStagingDirectory capability deliberately permits the
            // owning process to rename the directory after verification.
            .share_mode(share_mode);
    }

    options.open(path)
}

pub(crate) fn open_private_directory(path: &Path) -> io::Result<std::fs::File> {
    open_staged_directory_no_follow(path)
}

pub(crate) fn create_private_directory(path: &Path) -> PackageResult {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    #[cfg(target_os = "windows")]
    {
        create_private_windows_directory(path)?;
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        fs::create_dir(path)?;
    }
    Ok(())
}

#[allow(unsafe_code, reason = "Low-level staging directory validation")]
pub(crate) fn validate_private_directory(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> PackageResult {
    if !metadata.is_dir() || is_reparse_point(metadata) {
        return Err(format!(
            "private staging path is not a regular directory: {}",
            path.display()
        )
        .into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.mode() & 0o077 != 0 {
            return Err(format!(
                "private staging directory is accessible by other users: {}",
                path.display()
            )
            .into());
        }
        // SAFETY: geteuid has no preconditions and only reads the effective
        // user ID of the current process.
        let current_uid = unsafe { libc::geteuid() };
        if metadata.uid() != current_uid {
            return Err(format!(
                "private staging directory is not owned by the current user: {}",
                path.display()
            )
            .into());
        }
    }

    #[cfg(target_os = "windows")]
    validate_private_windows_directory(path)?;

    Ok(())
}

#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    reason = "Windows security API access for process token SID"
)]
pub(crate) fn current_windows_user_sid_string() -> PackageResult<String> {
    use crate::win32::{
        CloseHandle, GetCurrentProcess, GetLengthSid, GetTokenInformation, IsValidSid,
        OpenProcessToken, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use std::fmt::Write as _;
    use std::mem::{MaybeUninit, align_of, size_of};

    let mut token = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns the current process pseudo-handle and
    // `token` points to writable storage for the returned token handle.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error().into());
    }

    let result = (|| -> PackageResult<String> {
        let mut required = 0_u32;
        // SAFETY: the null buffer intentionally performs the documented size
        // query; `required` is writable output storage.
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
        };
        if required == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let required_size = required as usize;
        if required_size < size_of::<TOKEN_USER>() {
            return Err("token information is shorter than TOKEN_USER".into());
        }
        let word_size = size_of::<usize>();
        let words = required_size
            .checked_add(word_size - 1)
            .ok_or_else(|| "token information size overflow".to_owned())?
            / word_size;
        if align_of::<usize>() < align_of::<TOKEN_USER>() {
            return Err("token information alignment is unsupported".into());
        }
        let mut token_buffer = vec![MaybeUninit::<usize>::uninit(); words];
        // SAFETY: the storage is aligned at least to `TOKEN_USER`, has at
        // least the requested byte length, and remains alive for the call.
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let returned_size = required as usize;
        if returned_size < size_of::<TOKEN_USER>()
            || returned_size > token_buffer.len().saturating_mul(word_size)
        {
            return Err("token information exceeds its storage".into());
        }
        // SAFETY: the successful query populated a complete TOKEN_USER at the
        // aligned beginning of the storage, and the storage remains alive.
        let token_user = unsafe {
            token_buffer
                .as_ptr()
                .cast::<TOKEN_USER>()
                .as_ref()
                .ok_or_else(|| "token information buffer is null".to_owned())?
        };
        if token_user.User.Sid.is_null()
            // SAFETY: the SID pointer came from the successful token query and
            // is non-null before it is validated here.
            || unsafe { IsValidSid(token_user.User.Sid) == 0 }
        {
            return Err("current user token has an invalid SID".into());
        }
        // SAFETY: the SID has been validated and remains backed by the live
        // token information buffer for the duration of this read.
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) } as usize;
        if sid_length < 8 {
            return Err("current user SID is shorter than the fixed SID header".into());
        }
        // SAFETY: GetLengthSid returned the complete size of the validated SID,
        // which remains live in `token_buffer`.
        let sid =
            unsafe { std::slice::from_raw_parts(token_user.User.Sid.cast::<u8>(), sid_length) };
        let subauthority_count = sid[1] as usize;
        let expected_length = 8_usize
            .checked_add(
                subauthority_count
                    .checked_mul(4)
                    .ok_or_else(|| "current user SID length overflow".to_owned())?,
            )
            .ok_or_else(|| "current user SID length overflow".to_owned())?;
        if sid_length != expected_length {
            return Err("current user SID has an inconsistent length".into());
        }
        let identifier_authority =
            u64::from_be_bytes([0, 0, sid[2], sid[3], sid[4], sid[5], sid[6], sid[7]]);
        let mut value = format!("S-{}-{identifier_authority}", sid[0]);
        for index in 0..subauthority_count {
            let offset = 8 + index * 4;
            let subauthority = u32::from_le_bytes([
                sid[offset],
                sid[offset + 1],
                sid[offset + 2],
                sid[offset + 3],
            ]);
            write!(&mut value, "-{subauthority}")
                .map_err(|error| format!("failed to format current user SID: {error}"))?;
        }
        Ok(value)
    })();

    // SAFETY: `token` was returned by OpenProcessToken and is closed exactly
    // once after all token-backed pointers are no longer used.
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    reason = "Windows security API for ACL directory creation"
)]
pub(crate) fn create_private_windows_directory(path: &Path) -> PackageResult {
    use crate::win32::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, CreateDirectoryW, HLOCAL, LocalFree,
        SDDL_REVISION_1, SECURITY_ATTRIBUTES,
    };
    use std::os::windows::ffi::OsStrExt;

    // Do not inherit permissions from the temporary directory. Explicitly set
    // the current user as owner and grant access only to that SID and SYSTEM;
    // relying on the token's default owner can select the Administrators group
    // on hosted runners and make the private-directory invariant fail.
    let user_sid = current_windows_user_sid_string()?;
    let descriptor_string = wide_nul(&format!("O:{user_sid}D:P(A;;FA;;;SY)(A;;FA;;;{user_sid})"));
    let mut descriptor = std::ptr::null_mut::<std::ffi::c_void>();
    // SAFETY: the SDDL literal is NUL-terminated and `descriptor` points to
    // writable storage for the API-owned security descriptor pointer.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_string.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 || descriptor.is_null() {
        return Err(io::Error::last_os_error().into());
    }

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: both pointers refer to live, NUL-terminated buffers for the
    // duration of the call. The descriptor is freed below regardless of the
    // result, after CreateDirectoryW has consumed it synchronously.
    let created = unsafe {
        CreateDirectoryW(
            path_wide.as_ptr(),
            &security_attributes as *const SECURITY_ATTRIBUTES,
        )
    };
    let error = if created == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: the pointer was allocated by the conversion API and has not
    // been freed yet.
    unsafe {
        let _ = LocalFree(descriptor as HLOCAL);
    }
    error.map_or(Ok(()), Err).map_err(PackageError::from)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code, reason = "Windows security API for ACL validation")]
pub(crate) fn validate_private_windows_directory(path: &Path) -> PackageResult {
    // PrivateStagingDirectory::create supplies a protected DACL atomically.
    // Existing directories are accepted only through the explicit `open`
    // adoption path, which is used for OS-created temporary directories.
    use crate::win32::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, CloseHandle,
        ConvertStringSidToSidW, DACL_SECURITY_INFORMATION, ERROR_SUCCESS, EqualSid, GetAce,
        GetAclInformation, GetCurrentProcess, GetLengthSid, GetNamedSecurityInfoW,
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation, HLOCAL,
        IsValidSid, LocalFree, OWNER_SECURITY_INFORMATION, OpenProcessToken, SE_DACL_PROTECTED,
        SE_FILE_OBJECT, SECURITY_DESCRIPTOR_CONTROL, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use std::mem::{MaybeUninit, align_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::read_unaligned;

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut owner = std::ptr::null_mut::<std::ffi::c_void>();
    let mut dacl = std::ptr::null_mut::<ACL>();
    let mut security_descriptor = std::ptr::null_mut::<std::ffi::c_void>();
    let mut token = std::ptr::null_mut();
    let mut system_sid = std::ptr::null_mut::<std::ffi::c_void>();

    let result = (|| -> PackageResult {
        // SAFETY: `path_wide` is a live, NUL-terminated path buffer and all
        // output pointers refer to local storage owned for this call. The API
        // allocates `security_descriptor`, which is released below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut security_descriptor as *mut *mut std::ffi::c_void,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32).into());
        }

        // SAFETY: `GetCurrentProcess` returns the current process pseudo-handle
        // and `token` points to writable storage for the returned handle.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if opened == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut required = 0_u32;
        // SAFETY: the null buffer intentionally performs the documented size
        // query; `required` is writable output storage.
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
        };
        if required == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let required_size = required as usize;
        if required_size < size_of::<TOKEN_USER>() {
            return Err("token information is shorter than TOKEN_USER".into());
        }
        let word_size = size_of::<usize>();
        let words = required_size
            .checked_add(word_size - 1)
            .ok_or_else(|| "token information size overflow".to_owned())?
            / word_size;
        if align_of::<usize>() < align_of::<TOKEN_USER>() {
            return Err("token information alignment is unsupported".into());
        }
        let mut token_buffer = vec![MaybeUninit::<usize>::uninit(); words];
        // SAFETY: the storage is aligned at least to `TOKEN_USER`, has at
        // least the requested byte length, and remains alive for the call.
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let returned_size = required as usize;
        if returned_size < size_of::<TOKEN_USER>()
            || returned_size > token_buffer.len().saturating_mul(word_size)
        {
            return Err("token information exceeds its storage".into());
        }
        // SAFETY: the successful query populated a complete TOKEN_USER at the
        // aligned beginning of the storage, and the storage is still alive.
        let token_user = unsafe {
            token_buffer
                .as_ptr()
                .cast::<TOKEN_USER>()
                .as_ref()
                .ok_or_else(|| "token information buffer is null".to_owned())?
        };
        if owner.is_null()
            || token_user.User.Sid.is_null()
            // SAFETY: both SID pointers come from successful Windows security
            // APIs and are non-null before they are validated here.
            || unsafe { IsValidSid(owner) == 0 || IsValidSid(token_user.User.Sid) == 0 }
        {
            return Err("private staging directory has an invalid owner SID".into());
        }
        // SAFETY: the two SIDs were checked for non-null and validity above.
        if unsafe { EqualSid(owner, token_user.User.Sid) == 0 } {
            return Err("private staging directory is not owned by the current user".into());
        }

        let mut control = SECURITY_DESCRIPTOR_CONTROL::default();
        let mut revision = 0_u32;
        // SAFETY: `security_descriptor` was allocated and populated by
        // `GetNamedSecurityInfoW` above; the output pointers are local.
        let descriptor_ok = unsafe {
            GetSecurityDescriptorControl(security_descriptor, &mut control, &mut revision)
        };
        if descriptor_ok == 0 || control & SE_DACL_PROTECTED == 0 {
            return Err("private staging directory DACL is inherited or unavailable".into());
        }
        let mut dacl_present = 0;
        let mut dacl_defaulted = 0;
        let mut checked_dacl = std::ptr::null_mut::<ACL>();
        // SAFETY: the descriptor and output pointers are live, and the API
        // writes only to the local DACL metadata.
        let dacl_ok = unsafe {
            GetSecurityDescriptorDacl(
                security_descriptor,
                &mut dacl_present,
                &mut checked_dacl,
                &mut dacl_defaulted,
            )
        };
        if dacl_ok == 0 || dacl_present == 0 || checked_dacl.is_null() || checked_dacl != dacl {
            return Err("private staging directory DACL is unavailable".into());
        }
        let mut size_information = ACL_SIZE_INFORMATION::default();
        // SAFETY: `checked_dacl` was returned by the successful DACL query and
        // `size_information` is writable local output storage.
        let acl_ok = unsafe {
            GetAclInformation(
                checked_dacl,
                (&mut size_information as *mut ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        };
        if acl_ok == 0 || size_information.AceCount != 2 {
            return Err("private staging directory DACL contains unexpected entries".into());
        }
        let acl_bytes = size_information.AclBytesInUse as usize;
        if acl_bytes < size_of::<ACL>() {
            return Err("private staging directory DACL size is invalid".into());
        }

        let system_name = wide_nul("S-1-5-18");
        // SAFETY: the static SID string is NUL-terminated and `system_sid` is
        // writable storage for the API-owned SID pointer.
        let converted = unsafe { ConvertStringSidToSidW(system_name.as_ptr(), &mut system_sid) };
        if converted == 0 || system_sid.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let mut owner_ace = false;
        let mut system_ace = false;
        for index in 0..size_information.AceCount {
            let mut ace = std::ptr::null_mut::<std::ffi::c_void>();
            // SAFETY: `index` is bounded by the ACE count returned for the
            // validated ACL, and `ace` is writable output storage.
            if unsafe { GetAce(checked_dacl, index, &mut ace) == 0 } || ace.is_null() {
                return Err("private staging directory DACL ACE could not be read".into());
            }
            let acl_start = checked_dacl.cast::<u8>() as usize;
            let ace_start = ace.cast::<u8>() as usize;
            let ace_offset = ace_start.checked_sub(acl_start).ok_or_else(|| {
                "private staging directory DACL ACE is outside the ACL".to_owned()
            })?;
            let header_end = ace_offset
                .checked_add(size_of::<ACE_HEADER>())
                .ok_or_else(|| "private staging directory DACL ACE size overflow".to_owned())?;
            if header_end > acl_bytes {
                return Err("private staging directory DACL ACE header is truncated".into());
            }
            // SAFETY: the preceding range check covers the complete header;
            // `GetAce` may return an unaligned pointer, so read it unaligned.
            let header = unsafe { read_unaligned(ace.cast::<ACE_HEADER>()) };
            if header.AceType != 0 || header.AceFlags != 0 {
                return Err("private staging directory DACL contains a non-private ACE".into());
            }
            let ace_size = header.AceSize as usize;
            if ace_size < size_of::<ACCESS_ALLOWED_ACE>() {
                return Err("private staging directory DACL ACE is too short".into());
            }
            let ace_end = ace_offset
                .checked_add(ace_size)
                .ok_or_else(|| "private staging directory DACL ACE size overflow".to_owned())?;
            if ace_end > acl_bytes {
                return Err("private staging directory DACL ACE exceeds the ACL".into());
            }
            // SAFETY: the fixed ACCESS_ALLOWED_ACE prefix is fully covered by
            // the validated ACE range; use an unaligned copy rather than a
            // reference to API-provided bytes.
            let allowed = unsafe { read_unaligned(ace.cast::<ACCESS_ALLOWED_ACE>()) };
            if allowed.Mask == 0 {
                return Err("private staging directory DACL contains a zero-mask ACE".into());
            }
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            let sid_end = sid_offset
                .checked_add(8)
                .ok_or_else(|| "private staging directory ACE SID offset overflow".to_owned())?;
            if sid_end > ace_size {
                return Err("private staging directory ACE SID header is truncated".into());
            }
            // SAFETY: `sid_offset` is inside the validated ACE range and the
            // SID pointer is used only after Windows validates it below.
            let ace_sid = unsafe { ace.cast::<u8>().add(sid_offset).cast() };
            // SAFETY: the SID header lies inside the validated ACE. The API
            // checks the SID shape before its length is used for bounds.
            if unsafe { IsValidSid(ace_sid) == 0 } {
                return Err("private staging directory DACL ACE has an invalid SID".into());
            }
            // SAFETY: `ace_sid` is a valid SID pointer according to the API,
            // and its reported length must still fit within this ACE.
            let sid_length = unsafe { GetLengthSid(ace_sid) } as usize;
            if sid_length == 0 || sid_length > ace_size - sid_offset {
                return Err("private staging directory DACL ACE SID exceeds the ACE".into());
            }
            // SAFETY: `ace_sid`, `owner`, and `system_sid` have all passed SID
            // validation before comparison.
            owner_ace |= unsafe { EqualSid(ace_sid, owner) != 0 };
            // SAFETY: `ace_sid` and `system_sid` have both passed SID validation
            // before comparison.
            system_ace |= unsafe { EqualSid(ace_sid, system_sid) != 0 };
        }
        if !owner_ace || !system_ace {
            return Err("private staging directory DACL does not match the private policy".into());
        }
        Ok(())
    })();

    // SAFETY: each pointer is either null or owned by the corresponding
    // Windows API call above, and each resource is released at most once.
    unsafe {
        if !token.is_null() {
            let _ = CloseHandle(token);
        }
        if !system_sid.is_null() {
            let _ = LocalFree(system_sid as HLOCAL);
        }
        if !security_descriptor.is_null() {
            let _ = LocalFree(security_descriptor as HLOCAL);
        }
    }
    result
}

#[allow(
    clippy::permissions_set_readonly_false,
    reason = "Explicit non-readonly permission specification"
)]
pub(crate) fn manifest_permissions() -> PackageResult<std::fs::Permissions> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        Ok(std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let temporary = tempfile::Builder::new()
            .prefix(".xlfn-manifest-permissions-")
            .tempfile()?;
        let mut permissions = temporary.as_file().metadata()?.permissions();
        permissions.set_readonly(false);
        Ok(permissions)
    }
}

pub(crate) fn map_snapshot_open_error(target: &str, path: &Path, error: io::Error) -> PackageError {
    #[cfg(target_os = "windows")]
    {
        use crate::win32::ERROR_SHARING_VIOLATION;

        if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) {
            return PackageError::BundleSourceBusy {
                target: target.to_owned(),
                path: path.to_owned(),
                source: error,
            };
        }
    }

    PackageError::Message(format!(
        "{target}: failed to open {}: {error}",
        path.display()
    ))
}

pub(crate) fn unstable_bundle_source(target: &str, path: &Path) -> PackageError {
    PackageError::UnstableBundleSource {
        target: target.to_owned(),
        path: path.to_owned(),
    }
}

pub(crate) fn read_stable_snapshot(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    observer: &dyn SnapshotObserver,
) -> PackageResult<Arc<[u8]>> {
    read_stable_snapshot_with_limit(target, path, file, observer, None)
}

pub(crate) fn read_stable_snapshot_with_limit(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    observer: &dyn SnapshotObserver,
    maximum_len: Option<u64>,
) -> PackageResult<Arc<[u8]>> {
    let before = file_snapshot_state(file)?;
    if maximum_len.is_some_and(|maximum| before.len > maximum) {
        return Err(PackageError::Message(format!(
            "{target}: file exceeds the snapshot byte budget: {}",
            path.display()
        )));
    }
    let expected_len = usize::try_from(before.len).map_err(|_| {
        PackageError::Message(format!(
            "{target}: bundle file is too large to snapshot: {}",
            path.display()
        ))
    })?;

    let mut snapshot = Vec::new();
    snapshot.try_reserve_exact(expected_len).map_err(|_| {
        PackageError::Message(format!(
            "{target}: cannot reserve {expected_len} bytes for bundle snapshot: {}",
            path.display()
        ))
    })?;

    file.seek(SeekFrom::Start(0))?;
    let mut limited = file.take(before.len.saturating_add(1));
    let mut first_chunk = [0_u8; 64 * 1024];
    let count = limited.read(&mut first_chunk)?;
    if count != 0 {
        observer.after_first_chunk(path);
        if count > expected_len {
            return Err(unstable_bundle_source(target, path));
        }
        snapshot.extend_from_slice(&first_chunk[..count]);
    }

    let remaining = expected_len
        .checked_sub(snapshot.len())
        .ok_or_else(|| unstable_bundle_source(target, path))?;
    limited
        .by_ref()
        .take(remaining as u64)
        .read_to_end(&mut snapshot)?;
    if snapshot.len() != expected_len {
        return Err(unstable_bundle_source(target, path));
    }

    let mut extra = [0_u8; 1];
    if limited.read(&mut extra)? != 0 {
        return Err(unstable_bundle_source(target, path));
    }

    let after = file_snapshot_state(file)?;
    if before != after {
        return Err(unstable_bundle_source(target, path));
    }

    Ok(Arc::from(snapshot))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn verify_snapshot_against_second_read(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    expected: &[u8],
) -> PackageResult {
    let before = file_snapshot_state(file)?;
    file.seek(SeekFrom::Start(0))?;

    let expected_digest = Sha256::digest(expected);
    let mut hasher = Sha256::new();
    let mut limited = file.take(before.len.saturating_add(1));
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let observed_digest = hasher.finalize();
    let after = file_snapshot_state(file)?;
    if before != after || expected_digest != observed_digest {
        return Err(unstable_bundle_source(target, path));
    }

    Ok(())
}
