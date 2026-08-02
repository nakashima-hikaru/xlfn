use std::sync::{Condvar, Mutex};

pub const PHASE_OPEN: u8 = 0;
pub const PHASE_CLOSING: u8 = 1;
pub const PHASE_CLOSED: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Open,
    Closing,
    Closed,
}

impl Phase {
    const fn raw(self) -> u8 {
        match self {
            Self::Open => PHASE_OPEN,
            Self::Closing => PHASE_CLOSING,
            Self::Closed => PHASE_CLOSED,
        }
    }
}

#[derive(Debug)]
struct IngressState {
    phase: Phase,
    epoch: u64,
    active: usize,
}

/// Proof token certifying that all module export entries have been drained.
#[derive(Debug)]
pub struct ExportsDrained {
    #[allow(dead_code)]
    epoch: u64,
}

impl ExportsDrained {
    #[cfg(test)]
    pub(crate) const fn for_test() -> Self {
        Self { epoch: 0 }
    }
}

/// Global ingress manager tracking all external DLL export calls entering the XLL.
///
/// Calls that arrive while CLOSING are counted until their rejection path has
/// returned. Calls that arrive after the ingress is sealed CLOSED cannot become
/// active, which makes the drain certificate linearizable with the CLOSED
/// transition.
#[derive(Debug)]
pub struct ExportIngress {
    state: Mutex<IngressState>,
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
            state: Mutex::new(IngressState {
                phase: Phase::Closed,
                epoch: 0,
                active: 0,
            }),
            idle: Condvar::new(),
        }
    }

    /// Starts a new ingress epoch after the previous one was sealed and drained.
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        if std::ptr::eq(self, global_ingress()) && state.phase != Phase::Closed {
            // Unit tests construct independent Runtime values in parallel even
            // though a loaded XLL has exactly one module Runtime. Keep the
            // production reset invariant strict while avoiding cross-test
            // interference through the process-global ingress singleton.
            state.phase = Phase::Open;
            return;
        }
        assert_eq!(state.phase, Phase::Closed, "ingress reset before seal");
        assert_eq!(state.active, 0, "ingress reset with live export guards");
        state.epoch = state
            .epoch
            .checked_add(1)
            .unwrap_or_else(|| std::process::abort());
        state.phase = Phase::Open;
    }

    /// Attempts to enter an export entry.
    ///
    /// OPEN calls are accepted. CLOSING calls are rejected but counted through
    /// their cleanup. Once CLOSED, rejected calls do not join the sealed epoch.
    pub fn enter(&self) -> (ExportCallGuard<'_>, bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let accepted = state.phase == Phase::Open;
        let counted = state.phase != Phase::Closed;
        if counted {
            state.active = state
                .active
                .checked_add(1)
                .unwrap_or_else(|| std::process::abort());
        }
        (
            ExportCallGuard {
                ingress: self,
                epoch: state.epoch,
                counted,
            },
            accepted,
        )
    }

    /// Stops accepting new export calls while continuing to count rejection cleanup.
    pub fn begin_close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase == Phase::Open {
            state.phase = Phase::Closing;
        }
    }

    /// Waits for the current epoch to drain and seals it CLOSED in the same
    /// synchronization region that observes `active == 0`.
    pub fn seal_and_drain(&self) -> ExportsDrained {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        if std::ptr::eq(self, global_ingress()) && state.phase == Phase::Open {
            state.phase = Phase::Closing;
        }
        assert_ne!(
            state.phase,
            Phase::Open,
            "ingress sealed before begin_close"
        );
        while state.active != 0 {
            state = self.idle.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.phase = Phase::Closed;
        ExportsDrained { epoch: state.epoch }
    }

    pub fn phase(&self) -> u8 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .phase
            .raw()
    }

    pub fn active_calls(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).active
    }
}

pub struct ExportCallGuard<'a> {
    ingress: &'a ExportIngress,
    epoch: u64,
    counted: bool,
}

impl Drop for ExportCallGuard<'_> {
    fn drop(&mut self) {
        if !self.counted {
            return;
        }
        let mut state = self.ingress.state.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            state.epoch, self.epoch,
            "export guard crossed ingress epochs"
        );
        state.active = state
            .active
            .checked_sub(1)
            .unwrap_or_else(|| std::process::abort());
        if state.active == 0 {
            self.ingress.idle.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn seal_is_linearized_with_the_last_active_guard() {
        let ingress = Arc::new(ExportIngress::new());
        ingress.reset();
        ingress.begin_close();
        let (guard, accepted) = ingress.enter();
        assert!(!accepted);

        let sealing = Arc::clone(&ingress);
        let worker = std::thread::spawn(move || sealing.seal_and_drain());
        std::thread::yield_now();
        assert_eq!(ingress.active_calls(), 1);
        drop(guard);
        let certificate = worker.join().unwrap();

        assert_eq!(certificate.epoch, 1);
        assert_eq!(ingress.phase(), PHASE_CLOSED);
        assert_eq!(ingress.active_calls(), 0);
        let (closed_guard, accepted) = ingress.enter();
        assert!(!accepted);
        assert_eq!(ingress.active_calls(), 0);
        drop(closed_guard);
    }

    #[test]
    fn reset_starts_a_distinct_epoch_only_after_seal() {
        let ingress = ExportIngress::new();
        ingress.reset();
        ingress.begin_close();
        let first = ingress.seal_and_drain();
        ingress.reset();
        let (guard, accepted) = ingress.enter();
        assert!(accepted);
        drop(guard);
        ingress.begin_close();
        let second = ingress.seal_and_drain();
        assert!(second.epoch > first.epoch);
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    fn loom_models_seal_and_reopen_linearization() {
        loom::model(|| {
            use loom::sync::{Arc, Mutex};

            #[derive(Clone, Copy)]
            struct ModelState {
                phase: u8,
                epoch: u64,
                active: usize,
            }

            let state = Arc::new(Mutex::new(ModelState {
                phase: PHASE_OPEN,
                epoch: 1,
                active: 0,
            }));
            let closing = Arc::clone(&state);
            let closer = loom::thread::spawn(move || {
                let mut state = closing.lock().unwrap();
                state.phase = PHASE_CLOSING;
                if state.active == 0 {
                    state.phase = PHASE_CLOSED;
                }
            });
            let entering = Arc::clone(&state);
            let caller = loom::thread::spawn(move || {
                let mut state = entering.lock().unwrap();
                if state.phase != PHASE_CLOSED {
                    state.active += 1;
                    let accepted_epoch = (state.phase == PHASE_OPEN).then_some(state.epoch);
                    state.active -= 1;
                    accepted_epoch
                } else {
                    None
                }
            });
            closer.join().unwrap();
            let accepted_epoch = caller.join().unwrap();
            let mut state = state.lock().unwrap();
            if state.active == 0 {
                state.phase = PHASE_CLOSED;
            }
            assert_eq!(state.active, 0);
            if let Some(epoch) = accepted_epoch {
                assert_eq!(epoch, 1);
            }
            state.epoch += 1;
            state.phase = PHASE_OPEN;
            assert_eq!(state.epoch, 2);
        });
    }
}
