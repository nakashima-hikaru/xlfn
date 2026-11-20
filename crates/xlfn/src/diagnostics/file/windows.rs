//! Windows diagnostic log operations with pinned directory and file identities.

#![allow(unsafe_code, reason = "Windows file handle inspection and mutation")]

use super::{LogAccess, reject_parent_traversal};
use crate::win32::{
    BY_HANDLE_FILE_INFORMATION, DELETE, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
    ERROR_NOT_SUPPORTED, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_INFO_BY_HANDLE_CLASS,
    FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
    FileDispositionInfo, FileDispositionInfoEx, FileRenameInfo, GetFileInformationByHandle,
    SetFileInformationByHandle,
};
use std::ffi::OsStr;
use std::mem::{MaybeUninit, align_of, offset_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::{fs, io};

pub(super) struct LogDirectory {
    path: PathBuf,
    directory: fs::File,
    ancestors: Vec<fs::File>,
}

impl LogDirectory {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        reject_parent_traversal(path)?;
        let path = std::path::absolute(path)?;
        let paths: Vec<_> = path
            .ancestors()
            .filter(|ancestor| ancestor.has_root())
            .collect();
        let mut ancestors = Vec::with_capacity(paths.len());
        // Open from the root down. Ancestors remain pinned without delete
        // sharing while CreateFile resolves the next absolute path component.
        for ancestor in paths.into_iter().rev() {
            ancestors.push(open_directory(ancestor)?);
        }
        let directory = ancestors
            .pop()
            .ok_or_else(|| io::Error::other("log path has no root"))?;
        // Every component has been checked and remains pinned. Resolve the
        // spelling while those handles also prevent directory replacement.
        let path = path.canonicalize()?;
        Ok(Self {
            path,
            directory,
            ancestors,
        })
    }

    pub(super) fn ensure_subdirectory(mut self, name: &OsStr) -> io::Result<Self> {
        let path = self.path.join(name);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let directory = open_directory(&path)?;
        self.ancestors.push(self.directory);
        self.path = path;
        self.directory = directory;
        Ok(self)
    }

    pub(super) fn open_file(&self, name: &OsStr, access: LogAccess) -> io::Result<fs::File> {
        if name
            .encode_wide()
            .any(|unit| matches!(unit, 0 | 47 | 58 | 92))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log filename",
            ));
        }
        let mut options = pinned_open_options(FILE_FLAG_OPEN_REPARSE_POINT);
        match access {
            LogAccess::Lock => options.read(true).write(true).create(true),
            LogAccess::Append => options.append(true).create(true),
            LogAccess::Rotate => options.access_mode((FILE_READ_ATTRIBUTES | DELETE) as u32),
        };
        let file = options.open(self.path.join(name))?;
        if !file.metadata()?.is_file() || file_information(&file)?.nNumberOfLinks != 1 {
            return Err(io::Error::other(
                "log entry must be a regular file with one link",
            ));
        }
        Ok(file)
    }

    pub(super) fn rotate_file(
        &self,
        file: &fs::File,
        _source: &OsStr,
        destination: Option<&OsStr>,
    ) -> io::Result<()> {
        if file_information(file)?.nNumberOfLinks != 1 {
            return Err(io::Error::other("log rotation source must have one link"));
        }
        match destination {
            Some(name) => {
                // Use SetFileInformationByHandle's absolute-path form with a
                // NULL root. Every ancestor remains pinned without delete
                // sharing, and the source is still changed by handle.
                FileChange::Rename(&RenameInfo::new(&self.path, name)?).apply(file)
            }
            None => FileChange::Delete.apply(file),
        }
    }
}

fn pinned_open_options(flags: i32) -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options
        .custom_flags(flags as u32)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE) as u32);
    options
}

fn open_directory(path: &Path) -> io::Result<fs::File> {
    let file = pinned_open_options(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .access_mode((FILE_READ_ATTRIBUTES | FILE_TRAVERSE) as u32)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(io::Error::other("log parent must be a directory"));
    }
    file_information(&file)?;
    Ok(file)
}

fn file_information(file: &fs::File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `file` keeps the handle live and `info` can hold the output.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetFileInformationByHandle succeeded and initialized the output.
    let info = unsafe { info.assume_init() };
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT as u32 != 0 {
        return Err(io::Error::other("log entry is a reparse point"));
    }
    Ok(info)
}

// FILE_RENAME_INFO ends in a variable-length UTF-16 path. Pointer-sized words
// keep the header aligned while allowing paths longer than one basename.
struct RenameInfo {
    storage: Vec<usize>,
    bytes: u32,
}

const _: () = {
    assert!(align_of::<usize>() >= align_of::<FILE_RENAME_INFO>());
    assert!(offset_of!(FILE_RENAME_INFO, FileName).is_multiple_of(align_of::<u16>()));
};

impl RenameInfo {
    fn new(directory: &Path, name: &OsStr) -> io::Result<Self> {
        let count = name.encode_wide().count();
        if count == 0
            || count >= 256
            || name
                .encode_wide()
                .any(|unit| matches!(unit, 0 | 47 | 58 | 92))
            || matches!(name.to_str(), Some("." | ".."))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log archive name",
            ));
        }
        let path = directory.join(name);
        let count = path.as_os_str().encode_wide().count();
        if count >= 32768 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log archive path exceeds the Windows path limit",
            ));
        }
        let name_offset = offset_of!(FILE_RENAME_INFO, FileName);
        let bytes =
            (name_offset + (count + 1) * size_of::<u16>()).max(size_of::<FILE_RENAME_INFO>());
        let mut storage = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
        // SAFETY: storage is aligned for the header and covers the whole name
        // plus its terminator. The checked path bound keeps both lengths in
        // u32. Zero initialization leaves RootDirectory NULL and replacement
        // disabled.
        unsafe {
            (*storage.as_mut_ptr().cast::<FILE_RENAME_INFO>()).FileNameLength =
                (count * size_of::<u16>()) as u32;
        }
        // SAFETY: the checked name offset and length are aligned and contained
        // in storage. Derive the slice from the allocation, not FileName[1].
        let units = unsafe {
            std::slice::from_raw_parts_mut(
                storage
                    .as_mut_ptr()
                    .wrapping_byte_add(name_offset)
                    .cast::<u16>(),
                count + 1,
            )
        };
        for (out, unit) in units.iter_mut().zip(path.as_os_str().encode_wide()) {
            *out = unit;
        }
        Ok(Self {
            storage,
            bytes: bytes as u32,
        })
    }
}

enum FileChange<'a> {
    Rename(&'a RenameInfo),
    Delete,
}

impl FileChange<'_> {
    fn apply(self, file: &fs::File) -> io::Result<()> {
        let deleting = matches!(self, Self::Delete);
        // Remove the archive's name when this rotation handle closes, even
        // when a reader that shared DELETE still has the old contents open.
        // Otherwise the following no-replace rename sees a delete-pending
        // destination until that reader exits.
        let disposition = FILE_DISPOSITION_INFO_EX {
            Flags: (FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS) as u32,
        };
        let (class, buffer, bytes): (FILE_INFO_BY_HANDLE_CLASS, *const core::ffi::c_void, u32) =
            match self {
                Self::Rename(info) => (FileRenameInfo, info.storage.as_ptr().cast(), info.bytes),
                Self::Delete => (
                    FileDispositionInfoEx,
                    std::ptr::from_ref(&disposition).cast(),
                    size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
                ),
            };
        // SAFETY: the buffer matches `class` and remains live during the call.
        // The file and its directory remain open for this synchronous operation.
        if unsafe { SetFileInformationByHandle(file.as_raw_handle(), class, buffer, bytes) } == 0 {
            let error = io::Error::last_os_error();
            if !deleting
                || !matches!(
                    error.raw_os_error(),
                    Some(ERROR_INVALID_FUNCTION | ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED)
                )
            {
                return Err(error);
            }
            // Older filesystems may not support POSIX disposition. Retain
            // the existing conservative behavior there; never weaken sharing
            // or replace an occupied destination to force rotation through.
            let legacy = FILE_DISPOSITION_INFO { DeleteFile: true };
            // SAFETY: the live legacy buffer matches FileDispositionInfo and
            // the file remains pinned with DELETE access for this call.
            if unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle(),
                    FileDispositionInfo,
                    std::ptr::from_ref(&legacy).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
