use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

pub const PHASE_OPEN: u8 = 0;
pub const PHASE_CLOSING: u8 = 1;
pub const PHASE_CLOSED: u8 = 2;

/// Proof token certifying that all module export entries have been drained.
#[derive(Debug)]
pub struct ExportsDrained {
    _private: (),
}

impl ExportsDrained {
    #[cfg(test)]
    pub(crate) const fn for_test() -> Self {
        Self { _private: () }
    }
}

/// Global ingress manager tracking all external DLL export calls entering the XLL.
///
/// Both accepted calls and rejected calls (e.g. calls entering while closing)
/// are tracked by `ExportCallGuard` until the guard is dropped.
#[derive(Debug)]
pub struct ExportIngress {
    active: AtomicUsize,
    phase: AtomicU8,
    lock: Mutex<()>,
    idle: Condvar,
}

static GLOBAL_INGRESS: ExportIngress = ExportIngress::new();

pub fn global_ingress() -> &'static ExportIngress {
    &GLOBAL_INGRESS
}

impl Default for ExportIngress {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportIngress {
    pub const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            phase: AtomicU8::new(PHASE_OPEN),
            lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    /// Reset ingress phase to OPEN (used during open_addin).
    pub fn reset(&self) {
        self.phase.store(PHASE_OPEN, Ordering::SeqCst);
    }

    /// Attempts to enter an export entry.
    ///
    /// Returns `(guard, true)` if the ingress is open and the call is accepted.
    /// Returns `(guard, false)` if the ingress is closing/closed and rejected.
    ///
    /// In BOTH cases, the returned `ExportCallGuard` increments the active call
    /// count until it is dropped, ensuring that rejected call cleanup (such as
    /// returning a detached error) is fully tracked and drained during shutdown.
    pub fn enter(&self) -> (ExportCallGuard<'_>, bool) {
        self.active.fetch_add(1, Ordering::SeqCst);
        let accepted = self.phase.load(Ordering::SeqCst) == PHASE_OPEN;
        (ExportCallGuard { ingress: self }, accepted)
    }

    /// Transitions phase to CLOSING and waits for all active export entries
    /// (both accepted and rejected) to finish.
    pub fn close_and_drain(&self) -> ExportsDrained {
        self.phase.store(PHASE_CLOSING, Ordering::SeqCst);

        let mut lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        while self.active.load(Ordering::SeqCst) > 0 {
            lock = self.idle.wait(lock).unwrap_or_else(|e| e.into_inner());
        }

        self.phase.store(PHASE_CLOSED, Ordering::SeqCst);
        ExportsDrained { _private: () }
    }

    pub fn phase(&self) -> u8 {
        self.phase.load(Ordering::SeqCst)
    }

    pub fn active_calls(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }
}

pub struct ExportCallGuard<'a> {
    ingress: &'a ExportIngress,
}

impl<'a> Drop for ExportCallGuard<'a> {
    fn drop(&mut self) {
        if self.ingress.active.fetch_sub(1, Ordering::SeqCst) == 1 {
            let _lock = self.ingress.lock.lock().unwrap_or_else(|e| e.into_inner());
            self.ingress.idle.notify_all();
        }
    }
}
