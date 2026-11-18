//! Descriptor-relative diagnostic log operations on Unix.

#![allow(unsafe_code, reason = "Unix directory-relative file operations")]

use super::{LogAccess, reject_parent_traversal};
use std::ffi::{CStr, CString, OsStr};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::{fs, io};

#[cfg(target_os = "linux")]
const DIRECTORY_ACCESS: libc::c_int = libc::O_PATH;
#[cfg(target_os = "macos")]
const DIRECTORY_ACCESS: libc::c_int = libc::O_SEARCH;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const DIRECTORY_ACCESS: libc::c_int = libc::O_RDONLY;
const DIRECTORY_FLAGS: libc::c_int =
    DIRECTORY_ACCESS | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

pub(super) struct LogDirectory(fs::File);

impl LogDirectory {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        reject_parent_traversal(path)?;
        let root = if path.is_absolute() { c"/" } else { c"." };
        let mut directory = open_at(libc::AT_FDCWD, root, DIRECTORY_FLAGS)?;
        for component in path.components() {
            if let Component::Normal(name) = component {
                let name = CString::new(name.as_bytes())?;
                directory = open_at(directory.as_raw_fd(), &name, DIRECTORY_FLAGS)?;
            }
        }
        Ok(Self(directory))
    }

    pub(super) fn ensure_subdirectory(&self, name: &OsStr) -> io::Result<Self> {
        let name = CString::new(name.as_bytes())?;
        let parent = self.0.as_raw_fd();
        // SAFETY: `parent` is a live directory descriptor and `name` is a C string.
        if unsafe { libc::mkdirat(parent, name.as_ptr(), 0o700) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        open_at(parent, &name, DIRECTORY_FLAGS).map(Self)
    }

    pub(super) fn open_file(&self, name: &OsStr, access: LogAccess) -> io::Result<fs::File> {
        let name = CString::new(name.as_bytes())?;
        let access = match access {
            LogAccess::Lock => libc::O_RDWR | libc::O_CREAT,
            LogAccess::Append => libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            LogAccess::Rotate => libc::O_RDONLY,
        };
        let options = access | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        let file = open_at(self.0.as_raw_fd(), &name, options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::other(
                "log entry must be a regular file with one link",
            ));
        }
        Ok(file)
    }

    pub(super) fn rotate_file(
        &self,
        file: &fs::File,
        source: &OsStr,
        destination: Option<&OsStr>,
    ) -> io::Result<()> {
        let directory = self.0.as_raw_fd();
        let source = CString::new(source.as_bytes())?;
        let destination = destination
            .map(|name| CString::new(name.as_bytes()))
            .transpose()?;
        let metadata = file.metadata()?;
        let mut current = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the live directory and NUL-terminated name identify the entry;
        // `current` can hold the lstat-style result.
        if unsafe {
            libc::fstatat(
                directory,
                source.as_ptr(),
                current.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatat initialized the output structure.
        let current = unsafe { current.assume_init() };
        // Unix has no unlink/rename-by-handle equivalent. Compare identity and
        // link count immediately before the directory-relative mutation; the
        // pinned directory and cross-process log lock cover cooperating users.
        #[allow(
            clippy::unnecessary_cast,
            reason = "stat field widths differ across Unix targets"
        )]
        let identity = (current.st_dev as u64, current.st_ino as u64);
        if identity != (metadata.dev(), metadata.ino()) || current.st_nlink != 1 {
            return Err(io::Error::other("log rotation source changed"));
        }
        if let Some(destination) = destination {
            return rename_entry(directory, &source, &destination);
        }
        // SAFETY: `directory` is live and `source` is a NUL-terminated entry name.
        if unsafe { libc::unlinkat(directory, source.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn open_at(directory: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<fs::File> {
    // SAFETY: the directory is live or AT_FDCWD and `name` is NUL-terminated.
    // The mode argument is used only when the flags create a file.
    let fd = unsafe { libc::openat(directory, name.as_ptr(), flags, 0o600 as libc::c_int) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor, which the File now owns.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

fn rename_entry(directory: RawFd, source: &CStr, destination: &CStr) -> io::Result<()> {
    // SAFETY: both names are NUL-terminated and share a live directory descriptor.
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            directory,
            source.as_ptr(),
            directory,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    // SAFETY: both names are NUL-terminated and share a live directory descriptor.
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory,
            source.as_ptr(),
            directory,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    // SAFETY: both names are NUL-terminated and share a live directory descriptor.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let status =
        unsafe { libc::renameat(directory, source.as_ptr(), directory, destination.as_ptr()) };
    if status < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
