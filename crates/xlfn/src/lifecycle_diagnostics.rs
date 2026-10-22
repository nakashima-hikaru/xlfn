//! Passive diagnostic file writes, independent of the diagnostic worker being stopped.
//! No lifecycle decisions, Excel calls, background tasks or unload permissions live here.

use crate::lifecycle::LifecyclePhase;
use crate::shutdown_trace::ShutdownEvent;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

struct Sink {
    path: PathBuf,
    token: String,
    pid: u32,
    creation_ticks: u64,
}

impl Sink {
    fn from_config(config: &Value, pid: u32, creation_ticks: u64) -> Option<Self> {
        if config["schema"].as_u64()? != 1
            || config["pid"].as_u64()? != u64::from(pid)
            || config["creation_ticks"].as_u64()? != creation_ticks
        {
            return None;
        }
        let path = PathBuf::from(config["path"].as_str()?);
        let token = config["token"].as_str()?;
        if !path.is_absolute() || token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Self {
            path,
            token: token.to_owned(),
            pid,
            creation_ticks,
        })
    }

    fn discover() -> Option<Self> {
        let pid = std::process::id();
        let path = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("xlfn")
            .join("lifecycle-diagnostics")
            .join(format!("{pid}.json"));
        let mut config = String::new();
        File::open(path)
            .ok()?
            .take(16_384)
            .read_to_string(&mut config)
            .ok()?;
        let (_, creation_ticks) = native::identity()?;
        Self::from_config(
            &serde_json::from_str::<Value>(&config).ok()?,
            pid,
            creation_ticks,
        )
    }

    fn write(&self, event: &str, details: Value) {
        let Some((qpc_ticks, qpc_frequency, filetime_ticks, thread_id)) = native::stamp() else {
            return;
        };
        let line = json!({"schema": 1, "source": "xlfn", "pid": self.pid,
            "creation_ticks": self.creation_ticks, "token": self.token,
            "qpc_ticks": qpc_ticks, "qpc_frequency": qpc_frequency,
            "filetime_ticks": filetime_ticks, "thread_id": thread_id,
            "event": event, "details": details});
        // Reopen for each sparse lifecycle checkpoint. A killed process must not
        // lose a buffered trace, and the trace must not retain an async worker.
        if let Ok(mut file) = OpenOptions::new().append(true).open(&self.path) {
            let mut bytes = line.to_string().into_bytes();
            bytes.push(b'\n');
            let _ = file.write_all(&bytes);
        }
    }
}

static SINK: OnceLock<Option<Sink>> = OnceLock::new();
static INVOCATION: AtomicU64 = AtomicU64::new(1);

fn record(event: &str, details: Value) {
    if let Some(Some(sink)) = SINK.get() {
        sink.write(event, details);
    }
}

pub(crate) struct Boundary {
    name: &'static str,
    invocation: u64,
    finished: bool,
}

impl Boundary {
    pub(crate) fn enter(name: &'static str, phase: LifecyclePhase) -> Self {
        if SINK.get_or_init(Sink::discover).is_none() {
            return Self {
                name,
                invocation: 0,
                finished: true,
            };
        }
        let invocation = INVOCATION.fetch_add(1, Ordering::Relaxed);
        record(
            &format!("{name}.enter"),
            json!({"phase": format!("{phase:?}"), "invocation": invocation}),
        );
        Self {
            name,
            invocation,
            finished: false,
        }
    }

    pub(crate) fn finish(mut self, status: i32, phase: LifecyclePhase) {
        if self.finished {
            return;
        }
        record(
            &format!("{}.return", self.name),
            json!({"status": status, "phase": format!("{phase:?}"), "invocation": self.invocation}),
        );
        self.finished = true;
    }
}

impl Drop for Boundary {
    fn drop(&mut self) {
        if !self.finished {
            // This is NOT a completed-close certificate, including on panic.
            record(
                &format!("{}.scope_exit", self.name),
                json!({"unwinding": std::thread::panicking(), "invocation": self.invocation}),
            );
        }
    }
}

pub(crate) fn checkpoint(event: &ShutdownEvent) {
    if !matches!(SINK.get(), Some(Some(_))) {
        return;
    }
    // Never log per-call activity or thousands of individual registrations.
    if matches!(
        event,
        ShutdownEvent::BeginClose
            | ShutdownEvent::CallsDrained
            | ShutdownEvent::ReturnsDrained
            | ShutdownEvent::StopAsyncExecutor
            | ShutdownEvent::AsyncDrained
            | ShutdownEvent::SubscriptionsDrained
            | ShutdownEvent::CloseCallbackGate
            | ShutdownEvent::HostDetached
            | ShutdownEvent::ProveGenerationUnique
            | ShutdownEvent::ProveAddinQuiesced
            | ShutdownEvent::GenerationReclaimed
            | ShutdownEvent::HandlesDrained
            | ShutdownEvent::StopDiagnostics
            | ShutdownEvent::DiagnosticsDrained
            | ShutdownEvent::RtdDrained
            | ShutdownEvent::FinishClose
            | ShutdownEvent::Quarantine(_)
            | ShutdownEvent::FailStop(_)
    ) {
        record(
            "shutdown.checkpoint",
            json!({"checkpoint": format!("{event:?}")}),
        );
    }
}

#[cfg(windows)]
mod native {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    impl FileTime {
        fn ticks(&self) -> u64 {
            (u64::from(self.high) << 32) | u64::from(self.low)
        }
    }
    windows_link::link!("kernel32.dll" "system" fn GetCurrentProcess() -> *mut core::ffi::c_void);
    windows_link::link!("kernel32.dll" "system" fn GetCurrentThreadId() -> u32);
    windows_link::link!("kernel32.dll" "system" fn GetProcessTimes(process: *mut core::ffi::c_void, created: *mut FileTime, exited: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32);
    windows_link::link!("kernel32.dll" "system" fn QueryPerformanceCounter(value: *mut i64) -> i32);
    windows_link::link!("kernel32.dll" "system" fn QueryPerformanceFrequency(value: *mut i64) -> i32);
    windows_link::link!("kernel32.dll" "system" fn GetSystemTimePreciseAsFileTime(value: *mut FileTime));

    pub(super) fn identity() -> Option<(u32, u64)> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        // SAFETY: the current-process pseudo handle is always valid and is not closed.
        let process = unsafe { GetCurrentProcess() };
        // SAFETY: all outputs are writable FILETIME structures, valid for this call.
        let ok =
            unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
        (ok != 0).then(|| (std::process::id(), created.ticks()))
    }

    pub(super) fn stamp() -> Option<(i64, i64, u64, u32)> {
        let (mut counter, mut frequency, mut now) = (0, 0, FileTime::default());
        // SAFETY: the output points to writable LARGE_INTEGER-compatible storage.
        if unsafe { QueryPerformanceCounter(&mut counter) } == 0 {
            return None;
        }
        // SAFETY: the output points to writable LARGE_INTEGER-compatible storage.
        if unsafe { QueryPerformanceFrequency(&mut frequency) } == 0 {
            return None;
        }
        // SAFETY: now is writable FILETIME storage for the duration of the call.
        unsafe { GetSystemTimePreciseAsFileTime(&mut now) };
        // SAFETY: this read-only API takes no arguments.
        let thread = unsafe { GetCurrentThreadId() };
        Some((counter, frequency, now.ticks(), thread))
    }
}

#[cfg(not(windows))]
mod native {
    pub(super) fn identity() -> Option<(u32, u64)> {
        None
    }
    pub(super) fn stamp() -> Option<(i64, i64, u64, u32)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn native_identity_clock_and_file_trace_agree() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let (pid, created) = native::identity().unwrap();
        let sink = Sink::from_config(
            &json!({"schema": 1, "pid": pid, "creation_ticks": created,
            "token": "0123456789abcdef0123456789abcdef", "path": file.path()}),
            pid,
            created,
        )
        .unwrap();
        sink.write("xlAutoClose.enter", json!({"phase": "Open"}));
        sink.write(
            "xlAutoClose.return",
            json!({"phase": "Closed", "status": 1}),
        );
        let text = std::fs::read_to_string(file.path()).unwrap();
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["pid"], pid);
        assert_eq!(events[0]["creation_ticks"], created);
        assert!(events[0]["thread_id"].as_u64().unwrap() > 0);
        assert!(events[0]["qpc_frequency"].as_i64().unwrap() > 0);
        assert!(events[1]["qpc_ticks"].as_i64() >= events[0]["qpc_ticks"].as_i64());
        assert_eq!(events[1]["details"]["phase"], "Closed");
    }

    #[test]
    fn reused_pid_or_wrong_schema_cannot_enable_a_trace() {
        let config = json!({"schema": 1, "pid": 123, "creation_ticks": 456,
            "token": "0123456789abcdef0123456789abcdef", "path": std::env::temp_dir().join("lifecycle.jsonl")});
        assert!(Sink::from_config(&config, 123, 456).is_some());
        assert!(Sink::from_config(&config, 123, 457).is_none());
        assert!(Sink::from_config(&config, 124, 456).is_none());
        let mut invalid = config.clone();
        invalid["schema"] = json!(2);
        assert!(Sink::from_config(&invalid, 123, 456).is_none());
        invalid = config;
        invalid["path"] = json!("relative.jsonl");
        assert!(Sink::from_config(&invalid, 123, 456).is_none());
    }
}
