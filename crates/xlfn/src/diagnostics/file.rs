//! File-backed diagnostic sink and startup-log policy.

use super::{
    DIAGNOSTIC_TEXT_MAX_BYTES, DIAGNOSTIC_TRUNCATION_SUFFIX, LOG_GENERATIONS, LOG_MAX_BYTES,
};
use crate::diagnostics::event::{
    DiagnosticEvent, DiagnosticInitError, DiagnosticSink, FAILED_WRITES,
};
use crate::sync::Mutex;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::num::NonZeroU64;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
#[cfg(target_os = "windows")]
use std::time::SystemTime;
use std::{fs, io};

#[cfg_attr(unix, path = "file/unix.rs")]
#[cfg_attr(target_os = "windows", path = "file/windows.rs")]
mod log_directory;
use log_directory::LogDirectory;

struct FileDiagnosticRecord<'a> {
    timestamp_ms: u128,
    diagnostic_id: u64,
    udf: Cow<'a, str>,
    argument: Option<Cow<'a, str>>,
    error: String,
}

// Keep the production log schema independent of serde_derive. Verification
// traces and user value serialization enable that proc macro only when needed.
impl Serialize for FileDiagnosticRecord<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("FileDiagnosticRecord", 5)?;
        record.serialize_field("timestamp_ms", &self.timestamp_ms)?;
        record.serialize_field("diagnostic_id", &self.diagnostic_id)?;
        record.serialize_field("udf", &self.udf)?;
        record.serialize_field("argument", &self.argument)?;
        record.serialize_field("error", &self.error)?;
        record.end()
    }
}

#[cfg(any(target_os = "windows", test))]
struct StartupLogRecord<'a> {
    timestamp_ms: u128,
    message: Cow<'a, str>,
}

#[cfg(any(target_os = "windows", test))]
impl Serialize for StartupLogRecord<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("StartupLogRecord", 2)?;
        record.serialize_field("timestamp_ms", &self.timestamp_ms)?;
        record.serialize_field("message", &self.message)?;
        record.end()
    }
}

fn bounded_diagnostic_text(value: &str) -> Cow<'_, str> {
    if value.len() <= DIAGNOSTIC_TEXT_MAX_BYTES {
        return Cow::Borrowed(value);
    }

    let suffix = DIAGNOSTIC_TRUNCATION_SUFFIX;
    let prefix_limit = DIAGNOSTIC_TEXT_MAX_BYTES.saturating_sub(suffix.len());
    let mut prefix_end = prefix_limit.min(value.len());
    while prefix_end > 0 && !value.is_char_boundary(prefix_end) {
        prefix_end -= 1;
    }

    let mut bounded = String::with_capacity(prefix_end + suffix.len());
    bounded.push_str(&value[..prefix_end]);
    bounded.push_str(suffix);
    Cow::Owned(bounded)
}

/// Applies the log budget during formatting so a large vendor message never
/// creates an equally large temporary string before being truncated.
#[derive(Default)]
struct BoundedDiagnosticText {
    text: String,
    source_bytes: usize,
    prefix_bytes: usize,
    truncated: bool,
}

const DIAGNOSTIC_PREFIX_BYTES: usize =
    DIAGNOSTIC_TEXT_MAX_BYTES - DIAGNOSTIC_TRUNCATION_SUFFIX.len();

impl fmt::Write for BoundedDiagnosticText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let remaining = DIAGNOSTIC_TEXT_MAX_BYTES - self.source_bytes;
        let end = value.floor_char_boundary(remaining.min(value.len()));
        let accepted = &value[..end];
        self.text.reserve(end);
        if self.source_bytes < DIAGNOSTIC_PREFIX_BYTES {
            // Control characters may shrink. Save the normalized length at
            // the source boundary where a truncation suffix would begin.
            let prefix_limit = DIAGNOSTIC_PREFIX_BYTES - self.source_bytes;
            let prefix_end = accepted.floor_char_boundary(prefix_limit.min(end));
            let (prefix, tail) = accepted.split_at(prefix_end);
            self.push_sanitized(prefix);
            self.prefix_bytes = self.text.len();
            self.push_sanitized(tail);
        } else {
            self.push_sanitized(accepted);
        }
        self.source_bytes += end;

        if end == value.len() {
            return Ok(());
        }
        self.truncated = true;
        // Display implementations propagate this to stop formatting fields
        // whose output can no longer appear in the record.
        Err(fmt::Error)
    }
}

impl BoundedDiagnosticText {
    fn push_sanitized(&mut self, value: &str) {
        let mut start = 0;
        for (index, character) in value.char_indices() {
            if character.is_control() {
                self.text.push_str(&value[start..index]);
                self.text.push(' ');
                start = index + character.len_utf8();
            }
        }
        self.text.push_str(&value[start..]);
    }

    fn finish(mut self) -> String {
        if self.truncated {
            self.text.truncate(self.prefix_bytes);
            self.text.push_str(DIAGNOSTIC_TRUNCATION_SUFFIX);
        }
        self.text
    }
}

pub(crate) fn bounded_diagnostic_error(error: &crate::XllError) -> String {
    let mut output = BoundedDiagnosticText::default();
    let _ = fmt::write(
        &mut output,
        format_args!("{}", super::payload::TracedError(error)),
    );
    output.finish()
}

/// Size and retention settings for the add-in's diagnostic file sink.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticFileConfig {
    maximum_file_bytes: NonZeroU64,
    archives: u8,
}

impl DiagnosticFileConfig {
    /// Uses the default 4 MiB per file and retains three archives.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            maximum_file_bytes: NonZeroU64::new(LOG_MAX_BYTES).unwrap(),
            archives: LOG_GENERATIONS as u8,
        }
    }

    /// Sets the maximum file size in bytes, including each line terminator.
    /// A record larger than the limit counts as a failed diagnostic write.
    #[must_use]
    pub const fn with_maximum_file_bytes(mut self, limit: NonZeroU64) -> Self {
        self.maximum_file_bytes = limit;
        self
    }

    /// Sets the archive depth used on each rotation. Zero removes the
    /// outgoing active file instead of archiving it.
    #[must_use]
    pub const fn with_archives(mut self, count: u8) -> Self {
        self.archives = count;
        self
    }

    /// Returns the configured per-file size limit.
    #[must_use]
    pub const fn maximum_file_bytes(self) -> NonZeroU64 {
        self.maximum_file_bytes
    }

    /// Returns the configured number of rotated archives.
    #[must_use]
    pub const fn archives(self) -> u8 {
        self.archives
    }
}

impl Default for DiagnosticFileConfig {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct FileDiagnosticSink {
    pub(crate) log: Mutex<RotatingLog>,
}

impl DiagnosticSink for FileDiagnosticSink {
    fn report(&self, event: &DiagnosticEvent<'_>) {
        let timestamp = event
            .timestamp()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        let record = FileDiagnosticRecord {
            timestamp_ms: timestamp,
            diagnostic_id: event.diagnostic_id().as_u64(),
            udf: bounded_diagnostic_text(event.udf_id()),
            argument: event.argument().map(bounded_diagnostic_text),
            error: bounded_diagnostic_error(event.error()),
        };
        let line = match serde_json::to_string(&record) {
            Ok(line) => line,
            Err(_) => {
                FAILED_WRITES.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        if self.log.lock().write_line(&line).is_err() {
            FAILED_WRITES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) struct RotatingLog {
    pub(crate) path: PathBuf,
    directory: Option<LogDirectory>,
    file_name: OsString,
    pub(crate) maximum_bytes: u64,
    pub(crate) generations: usize,
}

fn reject_parent_traversal(path: &Path) -> io::Result<()> {
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log path contains a parent directory component",
        ));
    }
    Ok(())
}

enum LogAccess {
    Lock,
    Append,
    Rotate,
}

enum LogDirectoryRef<'a> {
    Owned(LogDirectory),
    Borrowed(&'a LogDirectory),
}

impl std::ops::Deref for LogDirectoryRef<'_> {
    type Target = LogDirectory;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owned(directory) => directory,
            Self::Borrowed(directory) => directory,
        }
    }
}

struct LogLock<'a> {
    file: fs::File,
    directory: LogDirectoryRef<'a>,
    name: &'a OsStr,
}

impl<'a> LogLock<'a> {
    fn acquire_in(directory: &'a LogDirectory, name: &'a OsStr) -> io::Result<Self> {
        let file = open_lock_file(directory, name)?;
        Ok(Self {
            file,
            directory: LogDirectoryRef::Borrowed(directory),
            name,
        })
    }

    fn acquire(path: &'a Path) -> io::Result<Self> {
        let (directory, name) = open_validated_directory(path)?;
        let file = open_lock_file(&directory, name)?;
        Ok(Self {
            file,
            directory: LogDirectoryRef::Owned(directory),
            name,
        })
    }

    fn open_active(&self) -> io::Result<fs::File> {
        self.directory.open_file(self.name, LogAccess::Append)
    }

    fn rotate(&self, generations: usize) -> io::Result<()> {
        let rotated = |generation: usize| {
            let mut name = self.name.to_os_string();
            name.push(format!(".{generation}"));
            name
        };
        if generations == 0 {
            return self.rotate_entry(self.name, None);
        }
        self.rotate_entry(&rotated(generations), None)?;
        for generation in (1..generations).rev() {
            self.rotate_entry(&rotated(generation), Some(&rotated(generation + 1)))?;
        }
        self.rotate_entry(self.name, Some(&rotated(1)))
    }

    fn rotate_entry(&self, name: &OsStr, destination: Option<&OsStr>) -> io::Result<()> {
        let file = match self.directory.open_file(name, LogAccess::Rotate) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        self.directory.rotate_file(
            &file,
            #[cfg(not(target_os = "windows"))]
            name,
            destination,
        )
    }
}

fn open_validated_directory(path: &Path) -> io::Result<(LogDirectory, &OsStr)> {
    reject_parent_traversal(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "log path has no filename"))?;
    let parent = path.parent().filter(|path| !path.as_os_str().is_empty());
    let directory = LogDirectory::open(parent.unwrap_or_else(|| Path::new(".")))?;
    Ok((directory, name))
}

fn open_lock_file(directory: &LogDirectory, name: &OsStr) -> io::Result<fs::File> {
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    let file = directory.open_file(&lock_name, LogAccess::Lock)?;
    // Logging is best effort: contention must never stall the worker or
    // startup. report() accounts this WouldBlock as a failed write.
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

impl Drop for LogLock<'_> {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl RotatingLog {
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_policy(path, LOG_MAX_BYTES, LOG_GENERATIONS)
    }

    #[cfg(test)]
    pub(crate) fn for_missing_path_test(
        path: PathBuf,
        maximum_bytes: u64,
        generations: usize,
    ) -> Self {
        Self {
            path,
            directory: None,
            file_name: OsString::new(),
            maximum_bytes,
            generations,
        }
    }

    pub(crate) fn open_with_policy(
        path: PathBuf,
        maximum_bytes: u64,
        generations: usize,
    ) -> io::Result<Self> {
        let (directory, name) = open_validated_directory(&path)?;
        let file_name = name.to_os_string();
        {
            let lock = LogLock::acquire_in(&directory, &file_name)?;
            let file = lock.open_active()?;
            if file.metadata()?.len() >= maximum_bytes {
                drop(file);
                lock.rotate(generations)?;
                lock.open_active()?;
            }
        }
        Ok(Self {
            path,
            directory: Some(directory),
            file_name,
            maximum_bytes,
            generations,
        })
    }

    pub(crate) fn write_line(&mut self, line: &str) -> io::Result<()> {
        let incoming = u64::try_from(line.len().saturating_add(1)).unwrap_or(u64::MAX);
        if incoming > self.maximum_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log record exceeds the maximum log size",
            ));
        }
        let lock = match &self.directory {
            Some(directory) => LogLock::acquire_in(directory, &self.file_name)?,
            None => LogLock::acquire(&self.path)?,
        };
        // Another instance or process may have appended or rotated since the
        // previous write. Resolve both the active file and its size while the
        // shared lock is held; retaining a file handle would target an archive.
        let mut file = lock.open_active()?;
        let size = file.metadata()?.len();
        if size > 0 && size.saturating_add(incoming) > self.maximum_bytes {
            drop(file);
            lock.rotate(self.generations)?;
            file = lock.open_active()?;
        }
        writeln!(file, "{line}")?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn append_startup_log(path: &Path, message: &str) -> io::Result<()> {
    let record = StartupLogRecord {
        timestamp_ms: SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis()),
        message: bounded_diagnostic_text(message),
    };
    let line = serde_json::to_string(&record).map_err(io::Error::other)?;
    RotatingLog::open(path.to_path_buf())?.write_line(&line)
}

/// Installs a basic failure log at `%LOCALAPPDATA%/<addin-id>/logs/diagnostics.log`.
pub(crate) fn install_file_diagnostic_sink(
    addin_id: &super::AddinId,
    config: DiagnosticFileConfig,
) -> Result<PathBuf, DiagnosticInitError> {
    let directory = log_directory(addin_id)?;
    open_file_sink(directory.join("diagnostics.log"), config)
}

fn validated_log_base(base: Option<OsString>) -> io::Result<PathBuf> {
    let base = base.map(PathBuf::from).filter(|path| {
        let valid =
            path.is_absolute() && !path.components().any(|part| part == Component::ParentDir);
        #[cfg(target_os = "windows")]
        let valid = valid
            && matches!(
                path.components().next(),
                Some(Component::Prefix(prefix)) if matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_)
                    | std::path::Prefix::VerbatimDisk(_)
                    | std::path::Prefix::UNC(..)
                    | std::path::Prefix::VerbatimUNC(..)
                )
            );
        valid
    });
    base.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "local app data must be an absolute path without parent traversal",
        )
    })
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code, reason = "Read and release the Windows Known Folder path")]
fn local_app_data() -> io::Result<PathBuf> {
    use crate::win32::{CoTaskMemFree, GUID, SHGetKnownFolderPath};
    use std::os::windows::ffi::OsStringExt;
    use std::ptr::null_mut;

    const LOCAL_APP_DATA: GUID = GUID {
        data1: 0xf1b3_2785,
        data2: 0x6fba,
        data3: 0x4fcf,
        data4: [0x9d, 0x55, 0x7b, 0x8e, 0x7f, 0x15, 0x70, 0x91],
    };

    struct KnownFolderPath(*mut u16);
    impl Drop for KnownFolderPath {
        fn drop(&mut self) {
            // SAFETY: the API transfers the allocation even on failure, and
            // CoTaskMemFree accepts null if no allocation was returned.
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }

    let mut path = KnownFolderPath(null_mut());
    // SAFETY: the GUID and output have live storage. A null token requests the
    // current user's folder.
    let status = unsafe { SHGetKnownFolderPath(&LOCAL_APP_DATA, 0, null_mut(), &mut path.0) };
    if status < 0 || path.0.is_null() {
        return Err(io::Error::other(format!(
            "SHGetKnownFolderPath for LocalAppData failed ({status:#010x})"
        )));
    }
    let mut len = 0;
    // SAFETY: success returned a NUL-terminated UTF-16 path owned by `path`.
    while unsafe { path.0.wrapping_add(len).read() } != 0 {
        len += 1;
    }
    // SAFETY: the scan counted the initialized path units before its terminator.
    let units = unsafe { std::slice::from_raw_parts(path.0, len) };
    // Resolve any user-profile redirection before validating the add-in's own
    // descendants, which must not traverse reparse points.
    validated_log_base(Some(OsString::from_wide(units)))?.canonicalize()
}

#[cfg(not(target_os = "windows"))]
fn local_app_data() -> io::Result<PathBuf> {
    validated_log_base(std::env::var_os("LOCALAPPDATA"))
}

pub(crate) fn log_directory(addin_id: &super::AddinId) -> io::Result<PathBuf> {
    // The user profile is the trust boundary. Never fall back to the current
    // directory or a shared temporary directory when its location is missing.
    let base = local_app_data()?;
    let directory =
        LogDirectory::open(&base)?.ensure_subdirectory(OsStr::new(addin_id.as_str()))?;
    directory.ensure_subdirectory(OsStr::new("logs"))?;
    Ok(base.join(addin_id.as_str()).join("logs"))
}

#[cfg(test)]
pub(crate) fn install_file_diagnostic_sink_at(
    path: PathBuf,
) -> Result<PathBuf, DiagnosticInitError> {
    open_file_sink(path, DiagnosticFileConfig::new())
}

pub(super) fn open_file_sink(
    path: PathBuf,
    config: DiagnosticFileConfig,
) -> Result<PathBuf, DiagnosticInitError> {
    // Creating or rotating the log is stateful. Do so only after the router
    // accepts the install; a duplicate must leave existing log files intact.
    super::install_diagnostic_sink_with(|| {
        let log = RotatingLog::open_with_policy(
            path.clone(),
            config.maximum_file_bytes().get(),
            usize::from(config.archives()),
        )?;
        Ok(FileDiagnosticSink {
            log: Mutex::new(log),
        })
    })?;
    Ok(path)
}

#[cfg(test)]
pub(super) fn test_log_directory() -> tempfile::TempDir {
    // On macOS the system temp path can traverse /var, a symlink.
    let base = std::env::temp_dir().canonicalize().unwrap();
    tempfile::tempdir_in(base).unwrap()
}

#[cfg(test)]
mod bounded_text_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn logs_reject_links_without_touching_their_targets() {
        let directory = test_log_directory();
        let target = directory.path().join("secret");
        fs::write(&target, "unchanged").unwrap();
        let log = directory.path().join("diagnostics.log");
        std::os::unix::fs::symlink(&target, &log).unwrap();
        assert!(RotatingLog::open(log).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "unchanged");
    }

    #[test]
    fn log_base_requires_an_explicit_absolute_path() {
        for value in [
            None,
            Some(""),
            Some("relative"),
            Some("C:relative"),
            Some("C:"),
        ] {
            assert!(validated_log_base(value.map(Into::into)).is_err());
        }
        let absolute = std::env::temp_dir();
        assert_eq!(
            validated_log_base(Some(absolute.clone().into_os_string())).unwrap(),
            absolute
        );
        #[cfg(not(target_os = "windows"))]
        assert!(validated_log_base(Some("/profile/local/../logs".into())).is_err());
        #[cfg(target_os = "windows")]
        {
            assert!(validated_log_base(Some(r"\relative".into())).is_err());
            assert!(validated_log_base(Some(r"\\.\DEVICE\folder".into())).is_err());
            assert!(validated_log_base(Some(r"C:\Users\example\..\logs".into())).is_err());
            assert!(validated_log_base(Some(r"C:\Users\example\AppData\Local".into())).is_ok());
            assert!(validated_log_base(Some(r"\\host\users\example\Local".into())).is_ok());
        }
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn log_rotation_requires_single_link_archives() {
        let directory = test_log_directory();
        let path = directory.path().join("diagnostics.log");
        let archive = path.with_file_name("diagnostics.log.1");
        let original = directory.path().join("original");
        let mut log = RotatingLog::open_with_policy(path.clone(), 8, 1).unwrap();
        log.write_line("first").unwrap();
        fs::write(&original, "unchanged").unwrap();
        fs::hard_link(&original, &archive).unwrap();
        assert!(log.write_line("next").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\n");
        assert_eq!(fs::read_to_string(&archive).unwrap(), "unchanged");
    }

    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    #[test]
    fn log_rotation_preserves_an_occupied_archive() {
        let directory = test_log_directory();
        let path = directory.path().join("diagnostics.log");
        let archive = path.with_file_name("diagnostics.log.1");
        fs::write(&path, "new").unwrap();
        fs::write(&archive, "older").unwrap();
        let lock = LogLock::acquire(&path).unwrap();

        let result = lock.rotate_entry(lock.name, archive.file_name());

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::read_to_string(&archive).unwrap(), "older");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn log_rotation_reuses_archive_name_while_a_delete_sharing_reader_is_open() {
        use crate::win32::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};
        use std::io::Read;
        use std::os::windows::fs::OpenOptionsExt;

        let directory = test_log_directory();
        let path = directory.path().join("diagnostics.log");
        let archive = path.with_file_name("diagnostics.log.1");
        let mut log = RotatingLog::open_with_policy(path.clone(), 8, 1).unwrap();
        log.write_line("first").unwrap();
        log.write_line("second").unwrap();
        let mut reader = fs::OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE) as u32)
            .open(&archive)
            .unwrap();

        log.write_line("third").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "third\n");
        assert_eq!(fs::read_to_string(&archive).unwrap(), "second\n");
        let mut original = String::new();
        reader.read_to_string(&mut original).unwrap();
        assert_eq!(original, "first\n");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn log_rotation_preserves_files_when_a_reader_denies_delete_sharing() {
        use crate::win32::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        use std::os::windows::fs::OpenOptionsExt;

        let directory = test_log_directory();
        let path = directory.path().join("diagnostics.log");
        let archive = path.with_file_name("diagnostics.log.1");
        let mut log = RotatingLog::open_with_policy(path.clone(), 8, 1).unwrap();
        log.write_line("first").unwrap();
        log.write_line("second").unwrap();
        let reader = fs::OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE) as u32)
            .open(&archive)
            .unwrap();

        assert!(log.write_line("third").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        assert_eq!(fs::read_to_string(&archive).unwrap(), "first\n");
        drop(reader);
        log.write_line("third").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "third\n");
        assert_eq!(fs::read_to_string(&archive).unwrap(), "second\n");
    }

    #[test]
    fn log_rotation_keeps_the_directory_pinned_until_unlock() {
        let directory = test_log_directory();
        let ancestor = directory.path().join("ancestor");
        let parent = ancestor.join("original");
        let moved = directory.path().join("moved");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join("diagnostics.log");
        fs::write(&path, "original").unwrap();
        let lock = LogLock::acquire(&path).unwrap();

        #[cfg(unix)]
        {
            fs::rename(&parent, &moved).unwrap();
            fs::create_dir(&parent).unwrap();
            fs::write(&path, "replacement").unwrap();
        }
        #[cfg(target_os = "windows")]
        {
            assert!(fs::rename(&parent, &moved).is_err());
            assert!(fs::rename(&ancestor, directory.path().join("moved-ancestor")).is_err());
        }

        lock.rotate(1).unwrap();
        drop(lock);
        #[cfg(unix)]
        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
        #[cfg(target_os = "windows")]
        fs::rename(&parent, &moved).unwrap();
        assert_eq!(
            fs::read_to_string(moved.join("diagnostics.log.1")).unwrap(),
            "original"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn log_rotation_supports_long_unicode_directory_paths() {
        use std::os::windows::ffi::OsStrExt;

        let directory = test_log_directory();
        let mut parent = directory.path().to_path_buf();
        for index in 0..3 {
            parent.push(format!("{index}-{}", "ログ🦀".repeat(20)));
            fs::create_dir(&parent).unwrap();
        }
        let path = parent.join("診断🦀.log");
        assert!(path.as_os_str().encode_wide().count() > 256);
        let mut log = RotatingLog::open_with_policy(path.clone(), 8, 2).unwrap();
        for line in ["first", "next", "last", "done"] {
            log.write_line(line).unwrap();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "done\n");
        assert_eq!(
            fs::read_to_string(parent.join("診断🦀.log.1")).unwrap(),
            "last\n"
        );
        assert_eq!(
            fs::read_to_string(parent.join("診断🦀.log.2")).unwrap(),
            "next\n"
        );
    }

    #[cfg(not(miri))]
    #[test]
    fn log_lock_serializes_separate_processes() {
        const PROBE: &str = "XLFN_TEST_LOG_LOCK_PROBE";
        if let Some(path) = std::env::var_os(PROBE) {
            let result = LogLock::acquire(Path::new(&path));
            assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
            println!("log lock was held by the parent process");
            return;
        }

        let directory = test_log_directory();
        let path = directory.path().join("diagnostics.log");
        let _lock = LogLock::acquire(&path).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "diagnostics::file::bounded_text_tests::log_lock_serializes_separate_processes",
                "--nocapture",
            ])
            .env(PROBE, &path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                child.wait().unwrap();
                panic!("an unsuccessful log lock attempt must return promptly");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("log lock was held by the parent process"));
    }

    #[test]
    fn log_contention_fails_promptly_and_recovery_preserves_rotation() {
        let directory = test_log_directory();
        let path = directory.path().join("diagnostics.log");
        let mut log = RotatingLog::open_with_policy(path.clone(), 8, 1).unwrap();
        log.write_line("first").unwrap();
        let held = LogLock::acquire(&path).unwrap();
        assert_eq!(
            log.write_line("lost").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(held);
        log.write_line("next").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "next\n");
        assert_eq!(
            fs::read_to_string(path.with_file_name("diagnostics.log.1")).unwrap(),
            "first\n"
        );
    }

    #[test]
    fn contended_diagnostic_event_is_counted_and_following_event_is_written() {
        let directory = test_log_directory();
        let path = directory.path().join("diagnostics.log");
        let sink = FileDiagnosticSink {
            log: Mutex::new(RotatingLog::open(path.clone()).unwrap()),
        };
        let error = crate::XllError::Panic;
        let event = DiagnosticEvent {
            udf_id: "contended_event",
            argument: None,
            error: &error,
            diagnostic_id: crate::diagnostics::id::from_u64(7),
            timestamp: std::time::SystemTime::UNIX_EPOCH,
        };
        let held = LogLock::acquire(&path).unwrap();
        let before = FAILED_WRITES.load(Ordering::Relaxed);
        sink.report(&event);
        assert!(FAILED_WRITES.load(Ordering::Relaxed) > before);
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        drop(held);
        sink.report(&event);
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        let record: serde_json::Value = serde_json::from_str(contents.trim_end()).unwrap();
        assert_eq!(record["udf"], "contended_event");
        assert_eq!(record["diagnostic_id"], 7);
    }

    #[test]
    fn diagnostic_json_preserves_schema_escaping_and_null_arguments() {
        let mut record = FileDiagnosticRecord {
            timestamp_ms: u128::MAX,
            diagnostic_id: u64::MAX,
            udf: Cow::Borrowed("日本語"),
            argument: Some(Cow::Owned("\"argument\"".to_owned())),
            error: "line\n\t\\".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            r#"{"timestamp_ms":340282366920938463463374607431768211455,"diagnostic_id":18446744073709551615,"udf":"日本語","argument":"\"argument\"","error":"line\n\t\\"}"#,
        );
        record.argument = None;
        record.timestamp_ms = 1;
        assert!(serde_json::to_value(&record).unwrap()["argument"].is_null());
    }

    #[test]
    fn startup_json_preserves_schema_and_unicode_text() {
        let record = StartupLogRecord {
            timestamp_ms: 123,
            message: Cow::Borrowed("日本語 \"quote\"\n"),
        };
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            r#"{"timestamp_ms":123,"message":"日本語 \"quote\"\n"}"#,
        );
    }

    #[test]
    fn formatting_matches_existing_truncation_at_unicode_boundaries() {
        for unit in ["x", "é", "日", "🦀"] {
            for extra in 0..8 {
                let message = unit.repeat(DIAGNOSTIC_TEXT_MAX_BYTES / unit.len() + extra);
                let error = crate::XllError::Native { code: 17, message };
                assert_eq!(
                    bounded_diagnostic_error(&error),
                    bounded_diagnostic_text(&error.to_string()),
                );
            }
        }
        let exact = "x".repeat(DIAGNOSTIC_TEXT_MAX_BYTES);
        let mut output = BoundedDiagnosticText::default();
        fmt::write(&mut output, format_args!("{exact}")).unwrap();
        assert_eq!(output.finish(), exact);
        assert!(matches!(
            bounded_diagnostic_text("udf"),
            Cow::Borrowed("udf")
        ));
        let error = crate::XllError::Native {
            code: 17,
            message: "a\n\u{0085}日本🦀".repeat(DIAGNOSTIC_TEXT_MAX_BYTES),
        };
        let expected: String = bounded_diagnostic_text(&error.to_string())
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        assert_eq!(bounded_diagnostic_error(&error), expected);
    }

    #[test]
    fn split_writes_preserve_source_byte_boundaries_after_sanitization() {
        use std::fmt::Write as _;

        let controls = "\u{0085}\u{009f}\n\t\0".repeat(16);
        let over_budget = "\n".repeat(DIAGNOSTIC_TRUNCATION_SUFFIX.len() + 1);
        for boundary in [DIAGNOSTIC_PREFIX_BYTES, DIAGNOSTIC_TEXT_MAX_BYTES] {
            for character in ["é", "\u{0085}", "🦀"] {
                for offset in 0..character.len() {
                    let padding = "x".repeat(boundary - controls.len() - offset);
                    for tail in ["", over_budget.as_str()] {
                        let source = [&controls, &padding, character, tail].concat();
                        let expected: String = bounded_diagnostic_text(&source)
                            .chars()
                            .map(|c| if c.is_control() { ' ' } else { c })
                            .collect();
                        let marker = controls.len() + padding.len();
                        for split in [0, controls.len(), marker, marker + character.len()] {
                            let mut output = BoundedDiagnosticText::default();
                            let result = output
                                .write_str(&source[..split])
                                .and_then(|()| output.write_str(&source[split..]));
                            assert_eq!(result.is_err(), source.len() > DIAGNOSTIC_TEXT_MAX_BYTES);
                            if result.is_err() {
                                assert!(output.write_str("ignored").is_err());
                            }
                            assert_eq!(output.finish(), expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn formatting_stops_when_the_record_budget_is_exhausted() {
        struct MustNotFormat;
        impl fmt::Display for MustNotFormat {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("formatting after the log budget must be skipped");
            }
        }
        let huge = "x".repeat(1024 * 1024);
        let mut output = BoundedDiagnosticText::default();
        assert!(fmt::write(&mut output, format_args!("{huge}{MustNotFormat}")).is_err());
        assert!(output.text.capacity() <= DIAGNOSTIC_TEXT_MAX_BYTES * 2);
        let result = output.finish();
        assert_eq!(result.len(), DIAGNOSTIC_TEXT_MAX_BYTES);
        assert!(result.ends_with(DIAGNOSTIC_TRUNCATION_SUFFIX));
    }
}
