//! File-backed diagnostic sink and startup-log policy.

use super::{
    DIAGNOSTIC_TEXT_MAX_BYTES, DIAGNOSTIC_TRUNCATION_SUFFIX, LOG_GENERATIONS, LOG_MAX_BYTES,
};
use crate::diagnostics::event::{
    DiagnosticEvent, DiagnosticInitError, DiagnosticSink, FAILED_WRITES,
};
use parking_lot::Mutex;
use serde::Serialize;
use std::borrow::Cow;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
#[cfg(target_os = "windows")]
use std::time::SystemTime;
use std::{fs, io};

#[derive(Serialize)]
struct FileDiagnosticRecord<'a> {
    timestamp_ms: u128,
    diagnostic_id: u64,
    udf: Cow<'a, str>,
    argument: Option<Cow<'a, str>>,
    error: String,
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
    truncated: bool,
}

impl fmt::Write for BoundedDiagnosticText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let remaining = DIAGNOSTIC_TEXT_MAX_BYTES - self.text.len();
        if value.len() <= remaining {
            self.text.push_str(value);
            return Ok(());
        }
        let end = value.floor_char_boundary(remaining.min(value.len()));
        self.text.push_str(&value[..end]);
        self.truncated = true;
        // Display implementations propagate this to stop formatting fields
        // whose output can no longer appear in the record.
        Err(fmt::Error)
    }
}

impl BoundedDiagnosticText {
    fn finish(mut self) -> String {
        if self.truncated {
            let limit = DIAGNOSTIC_TEXT_MAX_BYTES - DIAGNOSTIC_TRUNCATION_SUFFIX.len();
            self.text
                .truncate(self.text.floor_char_boundary(limit.min(self.text.len())));
            self.text.push_str(DIAGNOSTIC_TRUNCATION_SUFFIX);
        }
        self.text
    }
}

fn bounded_diagnostic_error(error: &crate::XllError) -> String {
    let mut output = BoundedDiagnosticText::default();
    let _ = fmt::write(&mut output, format_args!("{error}"));
    output.finish()
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
    pub(crate) maximum_bytes: u64,
    pub(crate) generations: usize,
}

struct LogLock(fs::File);

impl LogLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let mut lock_name = path.file_name().unwrap_or_default().to_os_string();
        lock_name.push(".lock");
        let lock_path = path.with_file_name(lock_name);
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for LogLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl RotatingLog {
    pub(crate) fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_policy(path, LOG_MAX_BYTES, LOG_GENERATIONS)
    }

    pub(crate) fn open_with_policy(
        path: PathBuf,
        maximum_bytes: u64,
        generations: usize,
    ) -> io::Result<Self> {
        let _lock = LogLock::acquire(&path)?;
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() >= maximum_bytes => {
                rotate_log_files(&path, generations)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
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
        let _lock = LogLock::acquire(&self.path)?;
        // Another instance or process may have appended or rotated since the
        // previous write. Resolve both the active file and its size while the
        // shared lock is held; retaining a file handle would target an archive.
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let size = file.metadata()?.len();
        if size > 0 && size.saturating_add(incoming) > self.maximum_bytes {
            drop(file);
            rotate_log_files(&self.path, self.generations)?;
            file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
        }
        writeln!(file, "{line}")?;
        Ok(())
    }
}

fn rotate_log_files(path: &Path, generations: usize) -> io::Result<()> {
    if generations == 0 {
        if fs::exists(path)? {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    let rotated = |generation: usize| {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{generation}"));
        path.with_file_name(name)
    };
    let oldest = rotated(generations);
    if fs::exists(&oldest)? {
        fs::remove_file(&oldest)?;
    }
    for generation in (1..generations).rev() {
        let source = rotated(generation);
        if fs::exists(&source)? {
            fs::rename(source, rotated(generation + 1))?;
        }
    }
    if fs::exists(path)? {
        fs::rename(path, rotated(1))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn append_startup_log(path: &Path, message: &str) -> io::Result<()> {
    #[derive(Serialize)]
    struct StartupLogRecord<'a> {
        timestamp_ms: u128,
        message: Cow<'a, str>,
    }

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
) -> Result<PathBuf, DiagnosticInitError> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = base.join(addin_id.as_str()).join("logs");
    fs::create_dir_all(&directory)?;
    install_file_diagnostic_sink_at(directory.join("diagnostics.log"))
}

pub(crate) fn install_file_diagnostic_sink_at(
    path: PathBuf,
) -> Result<PathBuf, DiagnosticInitError> {
    // Construct the replacement completely before touching the router. If file
    // creation or worker startup fails, the current healthy sink remains active.
    let sink = FileDiagnosticSink {
        log: Mutex::new(RotatingLog::open(path.clone())?),
    };
    super::set_diagnostic_sink(sink)?;
    Ok(path)
}

#[cfg(test)]
mod bounded_text_tests {
    use super::*;

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
