//! Windows diagnostic log operations with pinned directory and file identities.

#![allow(unsafe_code, reason = "Windows file handle inspection and mutation")]

use super::{LogAccess, reject_parent_traversal};
use crate::win32::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_INFO_BY_HANDLE_CLASS,
    FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
    FileDispositionInfo, FileRenameInfo, GetFileInformationByHandle, HANDLE,
    SetFileInformationByHandle,
};
use std::ffi::OsStr;
use std::mem::{MaybeUninit, offset_of};
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
            Some(name) => FileChange::Rename(&RenameInfo::new(&self.directory, name)?).apply(file),
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

// FILE_RENAME_INFO ends in a variable-length UTF-16 name. A bounded buffer
// suffices for one basename and avoids a dynamically aligned FFI allocation.
#[repr(C)]
struct RenameInfo {
    flags: u32,
    root_directory: HANDLE,
    name_length: u32,
    name: [u16; 256],
}

const _: () = {
    assert!(offset_of!(RenameInfo, flags) == offset_of!(FILE_RENAME_INFO, Anonymous));
    assert!(offset_of!(RenameInfo, root_directory) == offset_of!(FILE_RENAME_INFO, RootDirectory));
    assert!(offset_of!(RenameInfo, name_length) == offset_of!(FILE_RENAME_INFO, FileNameLength));
    assert!(offset_of!(RenameInfo, name) == offset_of!(FILE_RENAME_INFO, FileName));
};

impl RenameInfo {
    fn new(directory: &fs::File, name: &OsStr) -> io::Result<Self> {
        let mut info = Self {
            flags: 0,
            root_directory: directory.as_raw_handle(),
            name_length: 0,
            name: [0; 256],
        };
        let count = name.encode_wide().count();
        if count == 0 || count >= info.name.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log archive name",
            ));
        }
        for (out, unit) in info.name[..count].iter_mut().zip(name.encode_wide()) {
            *out = unit;
        }
        info.name_length = (count * size_of::<u16>()) as u32;
        Ok(info)
    }
}

enum FileChange<'a> {
    Rename(&'a RenameInfo),
    Delete,
}

impl FileChange<'_> {
    fn apply(self, file: &fs::File) -> io::Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        let (class, buffer, bytes): (FILE_INFO_BY_HANDLE_CLASS, *const core::ffi::c_void, u32) =
            match self {
                Self::Rename(info) => (
                    FileRenameInfo,
                    std::ptr::from_ref(info).cast(),
                    size_of::<RenameInfo>() as u32,
                ),
                Self::Delete => (
                    FileDispositionInfo,
                    std::ptr::from_ref(&disposition).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                ),
            };
        // SAFETY: the buffer matches `class` and remains live during the call.
        // The file and its directory remain open for this synchronous operation.
        if unsafe { SetFileInformationByHandle(file.as_raw_handle(), class, buffer, bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
