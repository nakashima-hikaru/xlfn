use crate::{IntoXllError, XllError, XllResult};
use parking_lot::{Mutex, RwLock};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::SystemTime;
use std::{fs, io};

const DIAGNOSTIC_QUEUE_CAPACITY: usize = 1024;
const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
const LOG_GENERATIONS: usize = 3;

/// Receives detailed failures while Excel continues to receive only safe error values.
pub trait DiagnosticSink: Send + Sync + 'static {
    /// Records one event and returns in bounded time.
    ///
    /// Delivery already occurs on a bounded framework worker, but XLL shutdown
    /// must join that worker. Implementations must not perform unbounded
    /// blocking; use an independently managed process for an uninterruptible
    /// remote or vendor sink.
    fn report(&self, event: &DiagnosticEvent<'_>);
}

/// A single failed framework or UDF invocation.
pub struct DiagnosticEvent<'a> {
    pub udf_id: &'static str,
    pub argument: Option<&'static str>,
    pub error: &'a XllError,
    pub diagnostic_id: u64,
    pub timestamp: SystemTime,
}

#[derive(Debug, thiserror::Error)]
pub enum DiagnosticInitError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("failed to start diagnostic logger worker: {0}")]
    WorkerSpawn(#[source] io::Error),
    #[error("diagnostic sink mutation was requested from its own worker")]
    ReentrantMutation,
    #[error("the diagnostic router is closing or closed")]
    RouterClosed,
    #[error("invalid addin id: {0:?}")]
    InvalidAddinId(#[from] InvalidAddinId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiagnosticShutdownError {
    #[error("diagnostic logger worker panicked")]
    WorkerPanicked,
    #[error("diagnostic logger cannot join itself")]
    ReentrantShutdown,
    #[error("the diagnostic router is closed")]
    RouterClosed,
    #[error("diagnostic router invariant violated")]
    InvariantViolation,
}

#[derive(Debug)]
pub struct DiagnosticsDrained {
    _private: (),
}

#[derive(Debug)]
pub(crate) struct DiagnosticsStopped {
    _private: (),
}

impl DiagnosticsStopped {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiagnosticPhase {
    Open,
    Closing,
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DiagnosticTerminalCloseError {
    #[error("diagnostic logger cannot join itself")]
    ReentrantShutdown,
    #[error("diagnostic router invariant violated during terminal close")]
    InvariantViolation,
}

impl IntoXllError for DiagnosticTerminalCloseError {
    fn into_xll_error(self) -> XllError {
        match self {
            Self::ReentrantShutdown => XllError::ReentrantCall,
            Self::InvariantViolation => XllError::Internal {
                diagnostic_id: 0x4449_4147_434c_4f53,
            },
        }
    }
}

struct DiagnosticRouter {
    sink: RwLock<Option<Arc<AsyncDiagnosticSink>>>,
    transition: Mutex<DiagnosticPhase>,
    retiring_workers: Mutex<Vec<std::thread::ThreadId>>,
}

impl DiagnosticRouter {
    fn caller_is_worker(&self) -> bool {
        let current = std::thread::current().id();
        if self
            .sink
            .read()
            .as_ref()
            .is_some_and(|sink| sink.worker_thread_id == current)
        {
            return true;
        }
        self.retiring_workers.lock().contains(&current)
    }

    fn mark_retiring(&self, sink: &AsyncDiagnosticSink) {
        let mut workers = self.retiring_workers.lock();
        if !workers.contains(&sink.worker_thread_id) {
            workers.push(sink.worker_thread_id);
        }
    }

    fn unmark_retiring(&self, sink: &AsyncDiagnosticSink) {
        self.retiring_workers
            .lock()
            .retain(|worker| *worker != sink.worker_thread_id);
    }

    #[cfg(test)]
    fn phase(&self) -> DiagnosticPhase {
        *self.transition.lock()
    }

    fn replace_with<F>(&self, make_sink: F) -> Result<(), DiagnosticInitError>
    where
        F: FnOnce() -> Result<Arc<AsyncDiagnosticSink>, DiagnosticInitError>,
    {
        if self.caller_is_worker() {
            return Err(DiagnosticInitError::ReentrantMutation);
        }
        let phase = self.transition.lock();
        // A transition may have retired the current worker while this caller
        // was waiting for the lock, so worker re-entry must be checked again.
        if self.caller_is_worker() {
            return Err(DiagnosticInitError::ReentrantMutation);
        }
        if *phase != DiagnosticPhase::Open {
            return Err(DiagnosticInitError::RouterClosed);
        }
        // Keep construction in the same linearization region as terminal
        // close. A close that wins the transition lock therefore cannot race a
        // worker that has been created but not yet published.
        let sink = make_sink()?;
        self.replace_locked(sink)
    }

    fn replace_locked(&self, sink: Arc<AsyncDiagnosticSink>) -> Result<(), DiagnosticInitError> {
        let previous = {
            let mut guard = self.sink.write();
            let previous = guard.replace(Arc::clone(&sink));
            if let Some(previous) = previous.as_ref() {
                self.mark_retiring(previous);
            }
            previous
        };

        let Some(previous) = previous else {
            return Ok(());
        };
        match previous.shutdown() {
            Ok(()) => {
                self.unmark_retiring(&previous);
                Ok(())
            }
            Err(DiagnosticShutdownError::ReentrantShutdown) => {
                {
                    let mut guard = self.sink.write();
                    self.mark_retiring(&sink);
                    *guard = Some(Arc::clone(&previous));
                }
                self.unmark_retiring(&previous);
                let _ = sink.shutdown();
                self.unmark_retiring(&sink);
                Err(DiagnosticInitError::ReentrantMutation)
            }
            Err(DiagnosticShutdownError::WorkerPanicked) => {
                self.unmark_retiring(&previous);
                sink.report(OwnedDiagnosticEvent {
                    udf_id: "diagnostic sink replacement",
                    argument: None,
                    error: XllError::Panic,
                    diagnostic_id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                    timestamp: SystemTime::now(),
                });
                Ok(())
            }
            Err(
                DiagnosticShutdownError::RouterClosed | DiagnosticShutdownError::InvariantViolation,
            ) => {
                self.unmark_retiring(&previous);
                Err(DiagnosticInitError::RouterClosed)
            }
        }
    }

    fn current(&self) -> Option<Arc<AsyncDiagnosticSink>> {
        self.sink.read().clone()
    }

    fn drain_reopenable(&self) -> Result<DiagnosticsDrained, DiagnosticShutdownError> {
        if self.caller_is_worker() {
            return Err(DiagnosticShutdownError::ReentrantShutdown);
        }
        let mut phase = self.transition.lock();
        if self.caller_is_worker() {
            return Err(DiagnosticShutdownError::ReentrantShutdown);
        }
        match *phase {
            DiagnosticPhase::Open => {}
            DiagnosticPhase::Closed => return Err(DiagnosticShutdownError::RouterClosed),
            DiagnosticPhase::Closing => {
                return Err(DiagnosticShutdownError::InvariantViolation);
            }
        }
        *phase = DiagnosticPhase::Closing;

        let previous = {
            let mut guard = self.sink.write();
            let previous = guard.take();
            if let Some(previous) = previous.as_ref() {
                self.mark_retiring(previous);
            }
            previous
        };

        let result = match previous {
            None => Ok(()),
            Some(previous) => match previous.shutdown() {
                Ok(()) => {
                    self.unmark_retiring(&previous);
                    Ok(())
                }
                Err(DiagnosticShutdownError::ReentrantShutdown) => {
                    *self.sink.write() = Some(Arc::clone(&previous));
                    self.unmark_retiring(&previous);
                    Err(DiagnosticShutdownError::ReentrantShutdown)
                }
                Err(error @ DiagnosticShutdownError::WorkerPanicked) => {
                    self.unmark_retiring(&previous);
                    Err(error)
                }
                Err(error) => {
                    self.unmark_retiring(&previous);
                    Err(error)
                }
            },
        };
        *phase = DiagnosticPhase::Open;
        result.map(|_| DiagnosticsDrained { _private: () })
    }

    fn close_terminal(
        &self,
    ) -> Result<crate::shutdown::StopOutcome<DiagnosticsStopped>, DiagnosticTerminalCloseError>
    {
        if self.caller_is_worker() {
            return Err(DiagnosticTerminalCloseError::ReentrantShutdown);
        }
        let mut phase = self.transition.lock();
        if self.caller_is_worker() {
            return Err(DiagnosticTerminalCloseError::ReentrantShutdown);
        }
        match *phase {
            DiagnosticPhase::Closed => {
                if self.sink.read().is_some() || !self.retiring_workers.lock().is_empty() {
                    return Err(DiagnosticTerminalCloseError::InvariantViolation);
                }
                return Ok(crate::shutdown::StopOutcome {
                    certificate: DiagnosticsStopped::new(),
                    issues: Vec::new(),
                });
            }
            DiagnosticPhase::Open => {}
            DiagnosticPhase::Closing => {
                return Err(DiagnosticTerminalCloseError::InvariantViolation);
            }
        }
        *phase = DiagnosticPhase::Closing;

        let previous = {
            let mut guard = self.sink.write();
            let previous = guard.take();
            if let Some(previous) = previous.as_ref() {
                self.mark_retiring(previous);
            }
            previous
        };
        let mut issues = Vec::new();
        if let Some(previous) = previous {
            match previous.shutdown() {
                Ok(()) => {}
                Err(DiagnosticShutdownError::WorkerPanicked) => {
                    // join() has returned, so the worker is no longer live.
                    // Preserve the stop certificate and report delivery loss.
                    issues.push(crate::shutdown::CleanupIssue {
                        component: "diagnostics",
                        kind: crate::shutdown::CleanupIssueKind::WorkerPanickedAfterJoin,
                        error: XllError::Panic,
                    });
                }
                Err(DiagnosticShutdownError::ReentrantShutdown) => {
                    *self.sink.write() = Some(Arc::clone(&previous));
                    self.unmark_retiring(&previous);
                    return Err(DiagnosticTerminalCloseError::ReentrantShutdown);
                }
                Err(_) => {
                    self.unmark_retiring(&previous);
                    return Err(DiagnosticTerminalCloseError::InvariantViolation);
                }
            }
            self.unmark_retiring(&previous);
        }
        if self.sink.read().is_some() || !self.retiring_workers.lock().is_empty() {
            return Err(DiagnosticTerminalCloseError::InvariantViolation);
        }
        *phase = DiagnosticPhase::Closed;
        Ok(crate::shutdown::StopOutcome {
            certificate: DiagnosticsStopped::new(),
            issues,
        })
    }

    fn reset(&self) -> XllResult<()> {
        let mut phase = self.transition.lock();
        if *phase != DiagnosticPhase::Closed
            || self.sink.read().is_some()
            || !self.retiring_workers.lock().is_empty()
        {
            return Err(XllError::Internal {
                diagnostic_id: 0x4449_4147_5253_4554,
            });
        }
        *phase = DiagnosticPhase::Open;
        Ok(())
    }
}

static ROUTER: OnceLock<DiagnosticRouter> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);
static FAILED_WRITES: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub(crate) static DIAGNOSTIC_TEST_MUTEX: Mutex<()> = Mutex::new(());

fn router() -> &'static DiagnosticRouter {
    ROUTER.get_or_init(|| DiagnosticRouter {
        sink: RwLock::new(None),
        transition: Mutex::new(DiagnosticPhase::Closed),
        retiring_workers: Mutex::new(Vec::new()),
    })
}

/// Installs or replaces the process-wide diagnostic sink.
///
/// Events are delivered by a bounded background worker. The replacement is
/// fully constructed and installed before the previous worker is flushed and
/// joined, so a terminal previous worker is never restored over a healthy sink.
pub fn set_diagnostic_sink(sink: impl DiagnosticSink) -> Result<(), DiagnosticInitError> {
    let sink: Arc<dyn DiagnosticSink> = Arc::new(sink);
    router().replace_with(|| AsyncDiagnosticSink::new(sink).map(Arc::new))
}

/// Flushes queued diagnostics, stops the logger worker, and releases its sink.
pub fn clear_diagnostic_sink() -> Result<DiagnosticsDrained, DiagnosticShutdownError> {
    router().drain_reopenable()
}

pub(crate) fn close_diagnostic_router()
-> Result<crate::shutdown::StopOutcome<DiagnosticsStopped>, DiagnosticTerminalCloseError> {
    router().close_terminal()
}

pub(crate) fn reset_diagnostic_router() -> XllResult<()> {
    router().reset()
}

/// Number of diagnostic events dropped because the bounded queue was full or closed.
#[must_use]
pub fn dropped_diagnostic_events() -> u64 {
    DROPPED_EVENTS.load(Ordering::Relaxed)
}

/// Number of file diagnostic deliveries that failed during write or rotation.
#[must_use]
pub fn failed_diagnostic_writes() -> u64 {
    FAILED_WRITES.load(Ordering::Relaxed)
}

struct OwnedDiagnosticEvent {
    udf_id: &'static str,
    argument: Option<&'static str>,
    error: XllError,
    diagnostic_id: u64,
    timestamp: SystemTime,
}

impl OwnedDiagnosticEvent {
    fn deliver(self, sink: &dyn DiagnosticSink) {
        let event = DiagnosticEvent {
            udf_id: self.udf_id,
            argument: self.argument,
            error: &self.error,
            diagnostic_id: self.diagnostic_id,
            timestamp: self.timestamp,
        };
        deliver_no_unwind(sink, &event);
    }
}

struct AsyncDiagnosticSink {
    sender: Mutex<Option<SyncSender<OwnedDiagnosticEvent>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    worker_thread_id: std::thread::ThreadId,
}

impl AsyncDiagnosticSink {
    fn new(sink: Arc<dyn DiagnosticSink>) -> Result<Self, DiagnosticInitError> {
        Self::new_named(sink, "xlfn-diagnostics")
    }

    fn new_named(
        sink: Arc<dyn DiagnosticSink>,
        worker_name: &str,
    ) -> Result<Self, DiagnosticInitError> {
        if worker_name.as_bytes().contains(&0) {
            return Err(DiagnosticInitError::WorkerSpawn(io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic worker name contains NUL",
            )));
        }
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<OwnedDiagnosticEvent>(DIAGNOSTIC_QUEUE_CAPACITY);
        let worker = std::thread::Builder::new()
            .name(worker_name.to_owned())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    event.deliver(&*sink);
                }
            })
            .map_err(DiagnosticInitError::WorkerSpawn)?;
        let worker_thread_id = worker.thread().id();
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_thread_id,
        })
    }

    fn is_current_thread_worker(&self) -> bool {
        std::thread::current().id() == self.worker_thread_id
    }

    fn report(&self, event: OwnedDiagnosticEvent) {
        let sender = self.sender.lock();
        let result = match sender.as_ref() {
            Some(sender) => sender.try_send(event),
            None => Err(TrySendError::Disconnected(event)),
        };
        if result.is_err() {
            DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn shutdown(&self) -> Result<(), DiagnosticShutdownError> {
        if self.is_current_thread_worker() {
            return Err(DiagnosticShutdownError::ReentrantShutdown);
        }
        self.sender.lock().take();
        let worker = self.worker.lock().take();
        if let Some(worker) = worker {
            worker
                .join()
                .map_err(|_| DiagnosticShutdownError::WorkerPanicked)?;
        }
        Ok(())
    }
}

impl Drop for AsyncDiagnosticSink {
    fn drop(&mut self) {
        if self.is_current_thread_worker() {
            return;
        }
        self.sender.get_mut().take();
        if let Some(worker) = self.worker.get_mut().take()
            && worker.join().is_err()
        {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::error!("diagnostic logger worker panicked during drop");
            }));
        }
    }
}

struct FileDiagnosticSink {
    log: Mutex<RotatingLog>,
}

impl DiagnosticSink for FileDiagnosticSink {
    fn report(&self, event: &DiagnosticEvent<'_>) {
        let timestamp = event
            .timestamp
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        let line = format!(
            "timestamp_ms={timestamp} diagnostic_id={} udf={} argument={:?} error={}",
            event.diagnostic_id, event.udf_id, event.argument, event.error
        );
        if self.log.lock().write_line(&line).is_err() {
            FAILED_WRITES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct RotatingLog {
    path: PathBuf,
    file: Option<fs::File>,
    size: u64,
    maximum_bytes: u64,
    generations: usize,
}

impl RotatingLog {
    fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_policy(path, LOG_MAX_BYTES, LOG_GENERATIONS)
    }

    fn open_with_policy(path: PathBuf, maximum_bytes: u64, generations: usize) -> io::Result<Self> {
        if fs::metadata(&path).is_ok_and(|metadata| metadata.len() >= maximum_bytes) {
            rotate_log_files(&path, generations)?;
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

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let incoming = u64::try_from(line.len().saturating_add(1)).unwrap_or(u64::MAX);
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
        if path.exists() {
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
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }
    for generation in (1..generations).rev() {
        let source = rotated(generation);
        if source.exists() {
            fs::rename(source, rotated(generation + 1))?;
        }
    }
    if path.exists() {
        fs::rename(path, rotated(1))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(crate) fn append_startup_log(path: &Path, message: &str) -> io::Result<()> {
    RotatingLog::open(path.to_path_buf())?.write_line(message)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AddinId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid addin id")]
pub struct InvalidAddinId;

impl AddinId {
    pub fn parse(value: &str) -> Result<Self, InvalidAddinId> {
        let trimmed = value.trim();
        if trimmed.is_empty()
            || trimmed.len() > 64
            || trimmed.contains([
                '/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0', '\r', '\n',
            ])
            || trimmed == "."
            || trimmed == ".."
            || trimmed.starts_with('.')
        {
            return Err(InvalidAddinId);
        }

        let upper = trimmed.to_ascii_uppercase();
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        if reserved.contains(&upper.as_str()) {
            return Err(InvalidAddinId);
        }

        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Installs a basic failure log at `%LOCALAPPDATA%/<addin-id>/logs/diagnostics.log`.
pub fn install_file_diagnostic_sink(addin_id: &str) -> Result<PathBuf, DiagnosticInitError> {
    let id = AddinId::parse(addin_id)?;
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = base.join(id.as_str()).join("logs");
    fs::create_dir_all(&directory)?;
    install_file_diagnostic_sink_at(directory.join("diagnostics.log"))
}

fn install_file_diagnostic_sink_at(path: PathBuf) -> Result<PathBuf, DiagnosticInitError> {
    // Construct the replacement completely before touching the router. If file
    // creation or worker startup fails, the current healthy sink remains active.
    let sink = FileDiagnosticSink {
        log: Mutex::new(RotatingLog::open(path.clone())?),
    };
    set_diagnostic_sink(sink)?;
    Ok(path)
}

/// Reports a failure without allowing tracing subscribers or user-provided sink code
/// to unwind into an Excel ABI entry point.
pub(crate) fn report_no_unwind(udf_id: &'static str, error: &XllError) -> u64 {
    let diagnostic_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let argument = match error {
        XllError::Input { argument, .. } => Some(*argument),
        _ => None,
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        tracing::event!(
            tracing::Level::ERROR,
            udf = udf_id,
            argument,
            diagnostic_id,
            error = %error,
            error_debug = ?error,
            "XLL invocation failed"
        );
        if let Some(sink) = router().current() {
            sink.report(OwnedDiagnosticEvent {
                udf_id,
                argument,
                error: error.clone(),
                diagnostic_id,
                timestamp: SystemTime::now(),
            });
        }
    }));
    diagnostic_id
}

fn deliver_no_unwind(sink: &dyn DiagnosticSink, event: &DiagnosticEvent<'_>) {
    let _ = catch_unwind(AssertUnwindSafe(|| sink.report(event)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct PanickingSink;

    impl DiagnosticSink for PanickingSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            panic!("injected diagnostic failure");
        }
    }

    #[test]
    fn panicking_tracing_subscriber_does_not_unwind_report_no_unwind() {
        let _test_guard = DIAGNOSTIC_TEST_MUTEX.lock();
        prepare_global_router();
        struct PanickingSubscriber;
        impl tracing::Subscriber for PanickingSubscriber {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {
                panic!("injected tracing subscriber panic");
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let dispatch = tracing::Dispatch::new(PanickingSubscriber);
        let _guard = tracing::dispatcher::set_default(&dispatch);

        let result = catch_unwind(AssertUnwindSafe(|| {
            report_no_unwind("panicking_subscriber_test", &XllError::Panic)
        }));
        assert!(
            result.is_ok(),
            "report_no_unwind must not unwind when tracing subscriber panics"
        );
    }

    struct ReentrantClearSink {
        clear_result: Arc<Mutex<Option<Result<(), DiagnosticShutdownError>>>>,
    }

    impl DiagnosticSink for ReentrantClearSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            let mut guard = self.clear_result.lock();
            if guard.is_none() {
                let res = clear_diagnostic_sink().map(|_| ());
                *guard = Some(res);
            }
        }
    }

    fn prepare_global_router() {
        let _ = close_diagnostic_router();
        router().reset().unwrap();
    }

    #[test]
    fn reentrant_clear_diagnostic_sink_returns_error_and_preserves_sink() {
        let _test_guard = DIAGNOSTIC_TEST_MUTEX.lock();
        prepare_global_router();
        let clear_result = Arc::new(Mutex::new(None));
        set_diagnostic_sink(ReentrantClearSink {
            clear_result: Arc::clone(&clear_result),
        })
        .unwrap();

        report_no_unwind("reentrant_test", &XllError::Panic);

        loop {
            if clear_result.lock().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let res = clear_result.lock().take().unwrap();
        assert_eq!(res, Err(DiagnosticShutdownError::ReentrantShutdown));

        // Clearing from the main thread succeeds (sink was preserved)
        assert!(clear_diagnostic_sink().is_ok());
    }

    struct ReentrantReplaceSink {
        replace_result: Arc<Mutex<Option<Result<(), DiagnosticInitError>>>>,
    }

    impl DiagnosticSink for ReentrantReplaceSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            let mut guard = self.replace_result.lock();
            if guard.is_none() {
                let res = set_diagnostic_sink(PanickingSink);
                *guard = Some(res);
            }
        }
    }

    #[test]
    fn reentrant_set_diagnostic_sink_returns_error_and_does_not_commit() {
        let _test_guard = DIAGNOSTIC_TEST_MUTEX.lock();
        prepare_global_router();
        let replace_result = Arc::new(Mutex::new(None));
        set_diagnostic_sink(ReentrantReplaceSink {
            replace_result: Arc::clone(&replace_result),
        })
        .unwrap();

        report_no_unwind("reentrant_replace_test", &XllError::Panic);

        loop {
            if replace_result.lock().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let res = replace_result.lock().take().unwrap();
        assert!(
            matches!(res, Err(DiagnosticInitError::ReentrantMutation)),
            "expected ReentrantMutation, got {res:?}"
        );

        // Clearing from the main thread succeeds since the original sink was preserved
        assert!(clear_diagnostic_sink().is_ok());
    }

    #[test]
    fn custom_sink_panic_is_contained() {
        let error = XllError::Panic;
        let event = DiagnosticEvent {
            udf_id: "test",
            argument: None,
            error: &error,
            diagnostic_id: 1,
            timestamp: SystemTime::now(),
        };
        deliver_no_unwind(&PanickingSink, &event);
    }

    #[test]
    fn worker_spawn_failure_is_returned_instead_of_panicking() {
        let error = AsyncDiagnosticSink::new_named(Arc::new(PanickingSink), "invalid\0name")
            .err()
            .unwrap();
        assert!(matches!(error, DiagnosticInitError::WorkerSpawn(_)));
    }

    #[test]
    fn worker_panic_is_observable_during_shutdown() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(|| panic!("injected diagnostic worker panic"));
        let worker_thread_id = worker.thread().id();
        let sink = AsyncDiagnosticSink {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_thread_id,
        };
        assert_eq!(
            sink.shutdown(),
            Err(DiagnosticShutdownError::WorkerPanicked)
        );
    }

    struct CountingSink(Arc<AtomicUsize>);

    impl DiagnosticSink for CountingSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn router_flushes_the_previous_sink_before_replacement() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let router = DiagnosticRouter {
            sink: RwLock::new(None),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
        };
        router
            .replace_with(|| {
                AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::clone(&first)))).map(Arc::new)
            })
            .unwrap();
        router.current().unwrap().report(OwnedDiagnosticEvent {
            udf_id: "reload",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: 1,
            timestamp: SystemTime::now(),
        });
        router
            .replace_with(|| {
                AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::clone(&second)))).map(Arc::new)
            })
            .unwrap();
        router.current().unwrap().report(OwnedDiagnosticEvent {
            udf_id: "reload",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: 2,
            timestamp: SystemTime::now(),
        });
        router.drain_reopenable().unwrap();
        assert_eq!(first.load(Ordering::Relaxed), 1);
        assert_eq!(second.load(Ordering::Relaxed), 1);
    }

    struct RetiringReentrySink {
        router: Arc<DiagnosticRouter>,
        started: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        result: std::sync::mpsc::SyncSender<Result<(), DiagnosticShutdownError>>,
    }

    impl DiagnosticSink for RetiringReentrySink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            let _ = self.started.send(());
            let _ = self.release.lock().recv();
            let _ = self.result.send(self.router.drain_reopenable().map(|_| ()));
        }
    }

    #[test]
    fn retiring_worker_reentry_is_rejected_before_waiting_for_transition() {
        let router = Arc::new(DiagnosticRouter {
            sink: RwLock::new(None),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
        });
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let old = Arc::new(
            AsyncDiagnosticSink::new(Arc::new(RetiringReentrySink {
                router: Arc::clone(&router),
                started: started_tx,
                release: Mutex::new(release_rx),
                result: result_tx,
            }))
            .unwrap(),
        );
        router.replace_with(|| Ok(old)).unwrap();
        router.current().unwrap().report(OwnedDiagnosticEvent {
            udf_id: "retiring",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: 1,
            timestamp: SystemTime::now(),
        });
        started_rx.recv().unwrap();

        let replacement = Arc::new(
            AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::new(AtomicUsize::new(0)))))
                .unwrap(),
        );
        let expected = Arc::clone(&replacement);
        let replacing_router = Arc::clone(&router);
        let replacing =
            std::thread::spawn(move || replacing_router.replace_with(|| Ok(replacement)));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if router
                .current()
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &expected))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "replacement did not publish before retirement"
            );
            std::thread::yield_now();
        }

        release_tx.send(()).unwrap();
        assert_eq!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            Err(DiagnosticShutdownError::ReentrantShutdown)
        );
        replacing.join().unwrap().unwrap();
        router.drain_reopenable().unwrap();
    }

    #[test]
    fn panicked_previous_worker_is_not_restored_over_a_healthy_replacement() {
        let worker = std::thread::spawn(|| panic!("injected diagnostic worker panic"));
        let worker_thread_id = worker.thread().id();
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let terminal = Arc::new(AsyncDiagnosticSink {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
            worker_thread_id,
        });
        let delivered = Arc::new(AtomicUsize::new(0));
        let replacement = Arc::new(
            AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::clone(&delivered)))).unwrap(),
        );
        let router = DiagnosticRouter {
            sink: RwLock::new(Some(terminal)),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
        };

        router
            .replace_with(|| Ok(Arc::clone(&replacement)))
            .unwrap();
        assert!(Arc::ptr_eq(&router.current().unwrap(), &replacement));
        router.current().unwrap().report(OwnedDiagnosticEvent {
            udf_id: "replacement",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: 1,
            timestamp: SystemTime::now(),
        });
        router.drain_reopenable().unwrap();
        assert_eq!(delivered.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn failed_file_sink_construction_preserves_the_current_sink() {
        let _test_guard = DIAGNOSTIC_TEST_MUTEX.lock();
        prepare_global_router();
        let delivered = Arc::new(AtomicUsize::new(0));
        set_diagnostic_sink(CountingSink(Arc::clone(&delivered))).unwrap();
        let directory = tempfile::tempdir().unwrap();

        let result = install_file_diagnostic_sink_at(directory.path().to_path_buf());
        assert!(matches!(result, Err(DiagnosticInitError::Io(_))));
        report_no_unwind("preserved", &XllError::Panic);
        clear_diagnostic_sink().unwrap();
        assert_eq!(delivered.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn file_delivery_failures_are_counted() {
        let before = failed_diagnostic_writes();
        let sink = FileDiagnosticSink {
            log: Mutex::new(RotatingLog {
                path: PathBuf::from("unavailable.log"),
                file: None,
                size: 0,
                maximum_bytes: LOG_MAX_BYTES,
                generations: LOG_GENERATIONS,
            }),
        };
        let error = XllError::Panic;
        sink.report(&DiagnosticEvent {
            udf_id: "failed_write",
            argument: None,
            error: &error,
            diagnostic_id: 1,
            timestamp: SystemTime::now(),
        });
        assert!(failed_diagnostic_writes() > before);
    }

    struct BlockingSink {
        first: AtomicBool,
        started: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl DiagnosticSink for BlockingSink {
        fn report(&self, _: &DiagnosticEvent<'_>) {
            if self.first.swap(false, Ordering::AcqRel) {
                let _ = self.started.send(());
                let _ = self.release.lock().recv();
            }
        }
    }

    fn wait_for_transition_lock(router: &DiagnosticRouter) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while router.transition.try_lock().is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "transition lock was not acquired"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn terminal_close_waiting_does_not_run_a_pending_install_factory() {
        let router = Arc::new(DiagnosticRouter {
            sink: RwLock::new(None),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
        });
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        router
            .replace_with(|| {
                AsyncDiagnosticSink::new(Arc::new(BlockingSink {
                    first: AtomicBool::new(true),
                    started: started_tx,
                    release: Mutex::new(release_rx),
                }))
                .map(Arc::new)
            })
            .unwrap();
        router.current().unwrap().report(OwnedDiagnosticEvent {
            udf_id: "terminal-close-race",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: 1,
            timestamp: SystemTime::now(),
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();

        let closing_router = Arc::clone(&router);
        let closing = std::thread::spawn(move || closing_router.close_terminal());
        wait_for_transition_lock(&router);

        let factory_calls = Arc::new(AtomicUsize::new(0));
        let factory_calls_for_install = Arc::clone(&factory_calls);
        let installing_router = Arc::clone(&router);
        let (install_started_tx, install_started_rx) = std::sync::mpsc::sync_channel(1);
        let installing = std::thread::spawn(move || {
            install_started_tx.send(()).unwrap();
            installing_router.replace_with(|| {
                factory_calls_for_install.fetch_add(1, Ordering::AcqRel);
                AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::new(AtomicUsize::new(0)))))
                    .map(Arc::new)
            })
        });
        install_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        std::thread::yield_now();
        assert_eq!(factory_calls.load(Ordering::Acquire), 0);

        release_tx.send(()).unwrap();
        let outcome = closing.join().unwrap().unwrap();
        let install_result = installing.join().unwrap();
        assert!(matches!(
            install_result,
            Err(DiagnosticInitError::RouterClosed)
        ));
        assert_eq!(factory_calls.load(Ordering::Acquire), 0);
        assert!(router.current().is_none());
        assert_eq!(router.phase(), DiagnosticPhase::Closed);
        assert!(router.retiring_workers.lock().is_empty());
        let _certificate = outcome.certificate;
    }

    #[test]
    fn install_that_linearizes_first_is_joined_by_terminal_close() {
        let router = Arc::new(DiagnosticRouter {
            sink: RwLock::new(None),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
        });
        let (factory_entered_tx, factory_entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_factory_tx, release_factory_rx) = std::sync::mpsc::sync_channel(1);
        let installing_router = Arc::clone(&router);
        let installing = std::thread::spawn(move || {
            installing_router.replace_with(|| {
                factory_entered_tx.send(()).unwrap();
                release_factory_rx.recv().unwrap();
                AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::new(AtomicUsize::new(0)))))
                    .map(Arc::new)
            })
        });
        factory_entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();

        let closing_router = Arc::clone(&router);
        let closing = std::thread::spawn(move || closing_router.close_terminal());
        wait_for_transition_lock(&router);
        release_factory_tx.send(()).unwrap();

        installing.join().unwrap().unwrap();
        let outcome = closing.join().unwrap().unwrap();
        assert!(outcome.issues.is_empty());
        assert!(router.current().is_none());
        assert_eq!(router.phase(), DiagnosticPhase::Closed);
        assert!(router.retiring_workers.lock().is_empty());
        let _certificate = outcome.certificate;
    }

    #[test]
    fn reset_requires_a_terminally_closed_router_before_reinstall() {
        let router = DiagnosticRouter {
            sink: RwLock::new(None),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
        };
        let _certificate = router.close_terminal().unwrap().certificate;

        let factory_calls = AtomicUsize::new(0);
        let result = router.replace_with(|| {
            factory_calls.fetch_add(1, Ordering::AcqRel);
            AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::new(AtomicUsize::new(0)))))
                .map(Arc::new)
        });
        assert!(matches!(result, Err(DiagnosticInitError::RouterClosed)));
        assert_eq!(factory_calls.load(Ordering::Acquire), 0);

        router.reset().unwrap();
        router
            .replace_with(|| {
                AsyncDiagnosticSink::new(Arc::new(CountingSink(Arc::new(AtomicUsize::new(0)))))
                    .map(Arc::new)
            })
            .unwrap();
        let _certificate = router.close_terminal().unwrap().certificate;
    }

    #[test]
    fn terminal_close_certifies_after_a_worker_panic() {
        let worker = std::thread::spawn(|| panic!("injected diagnostic worker panic"));
        let worker_thread_id = worker.thread().id();
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let router = DiagnosticRouter {
            sink: RwLock::new(Some(Arc::new(AsyncDiagnosticSink {
                sender: Mutex::new(Some(sender)),
                worker: Mutex::new(Some(worker)),
                worker_thread_id,
            }))),
            transition: Mutex::new(DiagnosticPhase::Open),
            retiring_workers: Mutex::new(Vec::new()),
        };

        let outcome = router.close_terminal().unwrap();
        assert!(outcome.issues.iter().any(|issue| {
            issue.kind == crate::shutdown::CleanupIssueKind::WorkerPanickedAfterJoin
        }));
        assert!(router.current().is_none());
        assert_eq!(router.phase(), DiagnosticPhase::Closed);
        assert!(router.retiring_workers.lock().is_empty());
        let _certificate = outcome.certificate;
    }

    #[test]
    fn full_diagnostic_queue_drops_instead_of_blocking_the_caller() {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let sink = AsyncDiagnosticSink::new(Arc::new(BlockingSink {
            first: AtomicBool::new(true),
            started: started_tx,
            release: Mutex::new(release_rx),
        }))
        .unwrap();
        let before = dropped_diagnostic_events();

        sink.report(OwnedDiagnosticEvent {
            udf_id: "bounded",
            argument: None,
            error: XllError::Panic,
            diagnostic_id: 1,
            timestamp: SystemTime::now(),
        });
        started_rx.recv().unwrap();
        for diagnostic_id in 2..=(DIAGNOSTIC_QUEUE_CAPACITY as u64 + 2) {
            sink.report(OwnedDiagnosticEvent {
                udf_id: "bounded",
                argument: None,
                error: XllError::Panic,
                diagnostic_id,
                timestamp: SystemTime::now(),
            });
        }
        assert!(dropped_diagnostic_events() > before);

        release_tx.send(()).unwrap();
        sink.shutdown().unwrap();
    }

    #[test]
    fn file_log_rotates_and_caps_generations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.log");

        for value in ["first-rotation", "second-rotation", "third-rotation"] {
            let mut log = RotatingLog::open_with_policy(path.clone(), 12, 2).unwrap();
            log.write_line(value).unwrap();
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), "third-rotation\n");
        assert_eq!(
            fs::read_to_string(path.with_file_name("diagnostics.log.1")).unwrap(),
            "second-rotation\n"
        );
        assert_eq!(
            fs::read_to_string(path.with_file_name("diagnostics.log.2")).unwrap(),
            "first-rotation\n"
        );
        assert!(!path.with_file_name("diagnostics.log.3").exists());
    }

    #[test]
    fn addin_id_rejects_path_traversal_and_reserved_names() {
        for invalid in [
            "..\\..\\outside",
            "../outside",
            "C:\\Windows",
            "\\\\server\\share",
            ".",
            "..",
            "CON",
            "PRN",
            "AUX",
            "NUL",
            "COM1",
            "LPT1",
            "bad/path",
            "bad\\path",
            "",
        ] {
            assert!(
                AddinId::parse(invalid).is_err(),
                "expected AddinId::parse({invalid:?}) to fail"
            );
        }

        assert!(AddinId::parse("valid-addin-id_123").is_ok());
    }
}
