//! File-backed diagnostic sink and startup-log policy.

use super::{
    DIAGNOSTIC_TEXT_MAX_BYTES, DIAGNOSTIC_TRUNCATION_SUFFIX, LOG_GENERATIONS, LOG_MAX_BYTES,
};
use crate::diagnostics::event::{
    DiagnosticEvent, DiagnosticInitError, DiagnosticSink, FAILED_WRITES,
};
use parking_lot::Mutex;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
#[cfg(target_os = "windows")]
use std::time::SystemTime;
use std::{fs, io};

#[derive(Serialize)]
struct FileDiagnosticRecord {
    timestamp_ms: u128,
    diagnostic_id: u64,
    udf: String,
    argument: Option<String>,
    error: String,
}

pub(crate) fn bounded_diagnostic_text(value: &str) -> String {
    if value.len() <= DIAGNOSTIC_TEXT_MAX_BYTES {
        return value.to_owned();
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
    bounded
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
            error: bounded_diagnostic_text(&event.error().to_string()),
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
    pub(crate) file: Option<fs::File>,
    pub(crate) size: u64,
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
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            file: Some(file),
            size,
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
        if self.size > 0 && self.size.saturating_add(incoming) > self.maximum_bytes {
            self.file.take();
            rotate_log_files(&self.path, self.generations)?;
            self.file = Some(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
            self.size = 0;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("rotating log file is unavailable after rotation"))?;
        writeln!(file, "{line}")?;
        self.size = self.size.saturating_add(incoming);
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
    struct StartupLogRecord {
        timestamp_ms: u128,
        message: String,
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
