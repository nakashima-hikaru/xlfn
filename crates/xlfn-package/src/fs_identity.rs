use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryIdentity(pub(crate) FileIdentity);
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileIdentity {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileIdentity {
    pub(crate) volume_serial_number: u64,
    pub(crate) file_id: [u8; 16],
}

#[cfg(not(any(unix, target_os = "windows")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileIdentity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileSnapshotState {
    pub(crate) identity: FileIdentity,
    pub(crate) len: u64,
    #[cfg(unix)]
    pub(crate) mtime: i64,
    #[cfg(unix)]
    pub(crate) mtime_nsec: i64,
    #[cfg(unix)]
    pub(crate) ctime: i64,
    #[cfg(unix)]
    pub(crate) ctime_nsec: i64,
    #[cfg(target_os = "windows")]
    pub(crate) last_write_time: u64,
}

#[cfg(unix)]
pub(crate) fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(FileSnapshotState {
        identity: FileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        },
        len: metadata.len(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code, reason = "Windows file handle information queries")]
pub(crate) fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    use crate::win32::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, HANDLE,
    };
    use std::os::windows::io::AsRawHandle;

    let handle = file.as_raw_handle() as HANDLE;
    // `BY_HANDLE_FILE_INFORMATION` exposes only a 64-bit file index. ReFS can
    // use 128-bit file IDs, so identity must come from `FILE_ID_INFO`.
    let mut identity = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: `handle` remains valid for the duration of the call because it is borrowed
    // from `file`, and `identity` points to writable storage of the required type.
    let status = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            identity.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if status == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `GetFileInformationByHandleEx` initializes the complete
    // `FILE_ID_INFO` output.
    let identity = unsafe { identity.assume_init() };

    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `handle` remains valid for the duration of the call because it is borrowed
    // from `file`, and `information` points to writable storage of the required type.
    let status = unsafe { GetFileInformationByHandle(handle, information.as_mut_ptr()) };
    if status == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `GetFileInformationByHandle` initializes the complete
    // `BY_HANDLE_FILE_INFORMATION` output structure.
    let information = unsafe { information.assume_init() };

    let len = (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow);
    let last_write_time = (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(information.ftLastWriteTime.dwLowDateTime);

    Ok(FileSnapshotState {
        identity: FileIdentity {
            volume_serial_number: identity.VolumeSerialNumber,
            file_id: identity.FileId.Identifier,
        },
        len,
        last_write_time,
    })
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    Ok(FileSnapshotState {
        identity: FileIdentity,
        len: file.metadata()?.len(),
    })
}

#[cfg(unix)]
pub(crate) fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    Ok(file_snapshot_state(left)?.identity == file_snapshot_state(right)?.identity)
}

#[cfg(target_os = "windows")]
pub(crate) fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    Ok(file_snapshot_state(left)?.identity == file_snapshot_state(right)?.identity)
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    // The supported release hosts are Unix and Windows. Keep other targets
    // conservative rather than claiming identity from path metadata alone.
    let _ = (left, right);
    Ok(false)
}

pub(crate) fn component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    // Both paths have already been canonicalized before this helper is used.
    // Comparing the native components directly avoids lossy UTF-16 decoding
    // and the incorrect ASCII-only case folding previously used here.
    left.as_os_str() == right.as_os_str()
}

pub(crate) fn reject_reparse_points(root: &Path, configured_path: &str) -> PackageResult {
    let mut current = root.to_path_buf();
    for component in Path::new(configured_path).components() {
        let Component::Normal(component) = component else {
            return Err(format!("bundle file has unsafe path {configured_path:?}").into());
        };
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current)
            && is_reparse_point(&metadata)
        {
            return Err(format!(
                "strict bundle path rejects symlink or reparse point: {}",
                current.display()
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}
