//! Benchmark-only gate control that never calls back into Excel.
//!
//! Excel can wait for native async results inside a COM calculation call, so
//! the thread releasing a gated result must not need that same COM apartment.

use std::{
    fs, io,
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use futures_channel::oneshot;

#[derive(Default)]
pub(super) struct AsyncControl {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Default)]
struct Shared {
    active: AtomicU64,
    finished: AtomicU64,
    started: AtomicU64,
    cancelled: AtomicU64,
    dropped: AtomicU64,
    released: AtomicBool,
    armed: AtomicBool,
    stop: AtomicBool,
    waiters: Mutex<Vec<oneshot::Sender<()>>>,
    wake: Condvar,
}

pub(super) struct ActiveGuard<'a> {
    shared: &'a Shared,
    completed: bool,
    cancelled: &'a xlfn::CancellationToken,
}

impl ActiveGuard<'_> {
    pub(super) fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        let armed = self.shared.armed.load(Ordering::Acquire);
        if armed && !self.completed {
            if self.cancelled.is_cancelled() {
                self.shared.cancelled.fetch_add(1, Ordering::Relaxed);
            } else {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.shared.finished.fetch_add(1, Ordering::Relaxed);
        self.shared.active.fetch_sub(1, Ordering::Release);
        if armed {
            self.shared.wake.notify_one();
        }
    }
}

impl AsyncControl {
    pub(super) fn enter<'a>(&'a self, cancelled: &'a xlfn::CancellationToken) -> ActiveGuard<'a> {
        let armed = self.shared.armed.load(Ordering::Acquire);
        if armed {
            self.shared.started.fetch_add(1, Ordering::Relaxed);
        }
        self.shared.active.fetch_add(1, Ordering::Release);
        if armed {
            self.shared.wake.notify_one();
        }
        ActiveGuard {
            shared: &self.shared,
            completed: false,
            cancelled,
        }
    }

    pub(super) fn active(&self) -> u64 {
        self.shared.active.load(Ordering::Acquire)
    }

    pub(super) fn finished(&self) -> u64 {
        self.shared.finished.load(Ordering::Acquire)
    }

    pub(super) fn wait(&self) -> Option<oneshot::Receiver<()>> {
        let mut waiters = self
            .shared
            .waiters
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if self.shared.released.load(Ordering::Acquire) {
            None
        } else {
            let (sender, receiver) = oneshot::channel();
            waiters.push(sender);
            Some(receiver)
        }
    }

    pub(super) fn release(&self) {
        release(&self.shared);
    }

    pub(super) fn arm(&self, directory: &Path, expected: u64) -> io::Result<()> {
        if !(1..=4096).contains(&expected) || !directory.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid gate directory or expected count",
            ));
        }
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if worker.is_some() || self.active() != 0 {
            return Err(io::Error::other(
                "gate is already armed or has active tasks",
            ));
        }
        for name in [
            "ready.json",
            "release",
            "released.json",
            "state.json",
            "control-error.txt",
        ] {
            if directory.join(name).exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "gate directory contains stale control files",
                ));
            }
        }
        self.shared.started.store(0, Ordering::Relaxed);
        self.shared.finished.store(0, Ordering::Relaxed);
        self.shared.cancelled.store(0, Ordering::Relaxed);
        self.shared.dropped.store(0, Ordering::Relaxed);
        self.shared.released.store(false, Ordering::Release);
        self.shared.armed.store(true, Ordering::Release);
        let shared = self.shared.clone();
        let directory = directory.to_path_buf();
        match thread::Builder::new()
            .name("benchmark async control".into())
            .spawn(move || {
                if let Err(error) = watch(&shared, &directory, expected) {
                    let _ = fs::write(directory.join("control-error.txt"), error.to_string());
                    // A broken observation channel must not strand Excel. The
                    // runner rejects missing acknowledgement/error files.
                    release(&shared);
                }
            }) {
            Ok(handle) => {
                *worker = Some(handle);
                Ok(())
            }
            Err(error) => {
                self.shared.armed.store(false, Ordering::Release);
                Err(error)
            }
        }
    }
}

impl Drop for AsyncControl {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        release(&self.shared);
        if let Some(worker) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

fn release(shared: &Shared) {
    let waiters = {
        let mut waiters = shared.waiters.lock().unwrap_or_else(|p| p.into_inner());
        shared.released.store(true, Ordering::Release);
        std::mem::take(&mut *waiters)
    };
    for waiter in waiters {
        let _ = waiter.send(());
    }
    shared.wake.notify_one();
}

fn write_json(directory: &Path, name: &str, value: &str) -> io::Result<()> {
    let temporary = directory.join(format!("{name}.tmp"));
    fs::write(&temporary, value)?;
    let destination = directory.join(name);
    for attempt in 0..20 {
        match fs::rename(&temporary, &destination) {
            Ok(()) => return Ok(()),
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(5 | 32))
                    && attempt < 19 =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded rename loop returns its final result")
}

fn watch(shared: &Shared, directory: &Path, expected: u64) -> io::Result<()> {
    let mut ready = false;
    let mut acknowledged = false;
    let mut previous = String::new();
    loop {
        let active = shared.active.load(Ordering::Acquire);
        if !ready && !shared.released.load(Ordering::Acquire) && active == expected {
            write_json(
                directory,
                "ready.json",
                &format!("{{\"active\":{active},\"expected\":{expected}}}"),
            )?;
            ready = true;
        }
        if !acknowledged && directory.join("release").is_file() {
            write_json(
                directory,
                "released.json",
                &format!("{{\"active_before_release\":{active}}}"),
            )?;
            release(shared);
            acknowledged = true;
        }
        let state = format!(
            "{{\"active\":{},\"expected\":{expected},\"started\":{},\"finished\":{},\"cancelled\":{},\"dropped\":{},\"released\":{},\"control_error\":null}}",
            shared.active.load(Ordering::Acquire),
            shared.started.load(Ordering::Relaxed),
            shared.finished.load(Ordering::Relaxed),
            shared.cancelled.load(Ordering::Relaxed),
            shared.dropped.load(Ordering::Relaxed),
            shared.released.load(Ordering::Acquire),
        );
        if state != previous {
            write_json(directory, "state.json", &state)?;
            previous = state;
        }
        if shared.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let waiters = shared.waiters.lock().unwrap_or_else(|p| p.into_inner());
        // A bounded wait also observes file creation and covers a notification
        // arriving between the atomic snapshot and this wait.
        drop(
            shared
                .wake
                .wait_timeout(waiters, Duration::from_millis(1))
                .unwrap_or_else(|p| p.into_inner()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "xlfn-gate-{}-{}",
                std::process::id(),
                NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn wait_file(path: &Path) -> String {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(value) = fs::read_to_string(path) {
                return value;
            }
            assert!(
                Instant::now() < until,
                "control file missing: {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn external_release_works_without_an_excel_callback() {
        let directory = Directory::new();
        let control = AsyncControl::default();
        control.arm(&directory.0, 2).unwrap();
        let mut first = control.wait().unwrap();
        let mut second = control.wait().unwrap();
        control.shared.started.store(2, Ordering::Relaxed);
        control.shared.active.store(2, Ordering::Release);
        assert_eq!(
            wait_file(&directory.0.join("ready.json")),
            "{\"active\":2,\"expected\":2}"
        );
        fs::write(directory.0.join("release"), "").unwrap();
        assert_eq!(
            wait_file(&directory.0.join("released.json")),
            "{\"active_before_release\":2}"
        );
        let until = Instant::now() + Duration::from_secs(5);
        while first.try_recv().unwrap().is_none() {
            assert!(Instant::now() < until);
            thread::yield_now();
        }
        while second.try_recv().unwrap().is_none() {
            assert!(Instant::now() < until);
            thread::yield_now();
        }
        assert!(control.wait().is_none());
    }

    #[test]
    fn emergency_release_does_not_claim_all_tasks_were_ready() {
        let directory = Directory::new();
        let control = AsyncControl::default();
        control.arm(&directory.0, 3).unwrap();
        control.shared.active.store(1, Ordering::Release);
        fs::write(directory.0.join("release"), "").unwrap();
        assert_eq!(
            wait_file(&directory.0.join("released.json")),
            "{\"active_before_release\":1}"
        );
        assert!(!directory.0.join("ready.json").exists());
    }

    #[test]
    fn arm_rejects_stale_files_and_invalid_capacity() {
        let directory = Directory::new();
        let control = AsyncControl::default();
        assert!(control.arm(&directory.0, 4097).is_err());
        fs::write(directory.0.join("release"), "").unwrap();
        assert!(control.arm(&directory.0, 1).is_err());
    }
}
