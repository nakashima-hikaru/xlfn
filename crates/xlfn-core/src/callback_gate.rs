use crate::ExcelCallbackStatus;
use parking_lot::{ReentrantMutex, ReentrantMutexGuard};
use std::cell::RefCell;
use xlfn_sys::XLRET_FAILED;

/// Module-wide admission state for every direct Excel C API callback.
///
/// The state is intentionally independent from a call-scoped
/// [`crate::host_callback::HostCallbackSession`]. A terminal result from one
/// lifecycle operation suppresses callbacks from every other execution source,
/// including worker-thread async completions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExcelCallbackGate {
    Open,
    Terminal(ExcelCallbackStatus),
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CallbackGateSuppressed {
    pub(crate) status: ExcelCallbackStatus,
}

pub(crate) struct CallbackGate {
    state: ReentrantMutex<RefCell<ExcelCallbackGate>>,
}

impl CallbackGate {
    const fn new(initial: ExcelCallbackGate) -> Self {
        Self {
            state: ReentrantMutex::new(RefCell::new(initial)),
        }
    }

    fn reset(&self) {
        *self.state.lock().borrow_mut() = ExcelCallbackGate::Open;
    }

    fn close(&self) {
        *self.state.lock().borrow_mut() = ExcelCallbackGate::Closed;
    }

    fn enter(&self) -> Result<CallbackGatePermit<'_>, CallbackGateSuppressed> {
        let lock = self.state.lock();
        let state = *lock.borrow();
        match state {
            ExcelCallbackGate::Open => Ok(CallbackGatePermit { lock }),
            ExcelCallbackGate::Terminal(status) => Err(CallbackGateSuppressed { status }),
            ExcelCallbackGate::Closed => Err(CallbackGateSuppressed {
                status: ExcelCallbackStatus::Failed(XLRET_FAILED),
            }),
        }
    }

    fn blocked_status(&self) -> Option<ExcelCallbackStatus> {
        callback_blocked_status(&self.state.lock())
    }

    #[cfg(test)]
    fn observe(&self, status: ExcelCallbackStatus) {
        observe_state(&self.state.lock(), status);
    }
}

/// Keeps the module callback gate held across one direct Excel C API call.
/// Reentrant acquisition is allowed for host callbacks that synchronously
/// re-enter this XLL on the same thread; callbacks from other threads wait.
pub(crate) struct CallbackGatePermit<'a> {
    lock: ReentrantMutexGuard<'a, RefCell<ExcelCallbackGate>>,
}

impl CallbackGatePermit<'_> {
    pub(crate) fn observe(&self, status: ExcelCallbackStatus) {
        observe_state(&self.lock, status);
    }
}

fn callback_blocked_status(state: &RefCell<ExcelCallbackGate>) -> Option<ExcelCallbackStatus> {
    match *state.borrow() {
        ExcelCallbackGate::Open => None,
        ExcelCallbackGate::Terminal(status) => Some(status),
        ExcelCallbackGate::Closed => Some(ExcelCallbackStatus::Failed(XLRET_FAILED)),
    }
}

fn observe_state(state: &RefCell<ExcelCallbackGate>, status: ExcelCallbackStatus) {
    if !status.is_terminal() {
        return;
    }
    let mut gate = state.borrow_mut();
    if matches!(*gate, ExcelCallbackGate::Open) {
        *gate = ExcelCallbackGate::Terminal(status);
    }
}

static MODULE_CALLBACK_GATE: CallbackGate = CallbackGate::new(ExcelCallbackGate::Closed);

pub(crate) fn reset() {
    MODULE_CALLBACK_GATE.reset();
}

pub(crate) fn reset_from_runtime() {
    #[cfg(test)]
    let Some(_test_guard) = crate::test_callback::try_lock() else {
        // A callback fixture owned by another test has the module gate as its
        // sole test process state. Unrelated Runtime fixtures must not mutate
        // that state while the fixture is active.
        return;
    };
    reset();
}

pub(crate) fn close_from_runtime() {
    #[cfg(test)]
    let Some(_test_guard) = crate::test_callback::try_lock() else {
        // See `reset_from_runtime`: unrelated test Runtime instances must not
        // mutate a callback fixture's process-wide gate.
        return;
    };
    MODULE_CALLBACK_GATE.close();
}

pub(crate) fn enter() -> Result<CallbackGatePermit<'static>, CallbackGateSuppressed> {
    MODULE_CALLBACK_GATE.enter()
}

pub(crate) fn blocked_status() -> Option<ExcelCallbackStatus> {
    MODULE_CALLBACK_GATE.blocked_status()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use xlfn_sys::{XLRET_ABORT, XLRET_UNCALCED};

    #[test]
    fn runtime_transitions_update_the_module_gate() {
        let _test_guard = crate::test_callback::lock();
        reset_from_runtime();
        assert!(enter().is_ok());
        close_from_runtime();
        assert!(matches!(enter(), Err(CallbackGateSuppressed { .. })));
    }

    #[test]
    fn terminal_status_is_module_wide_and_close_is_final() {
        let gate = CallbackGate::new(ExcelCallbackGate::Closed);
        gate.reset();
        let permit = gate.enter().unwrap();

        permit.observe(ExcelCallbackStatus::from_raw(XLRET_ABORT));
        drop(permit);
        let suppressed = match gate.enter() {
            Ok(_) => panic!("terminal gate unexpectedly admitted a callback"),
            Err(suppressed) => suppressed,
        };
        assert_eq!(
            suppressed,
            CallbackGateSuppressed {
                status: ExcelCallbackStatus::Abort,
            }
        );

        // A later terminal status cannot replace the first one, preserving the
        // status that caused the module-wide suppression.
        gate.observe(ExcelCallbackStatus::from_raw(XLRET_UNCALCED));
        assert_eq!(gate.blocked_status(), Some(ExcelCallbackStatus::Abort));

        gate.close();
        assert_eq!(
            gate.blocked_status(),
            Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
        );
    }

    #[test]
    fn permit_serializes_callbacks_until_terminal_status_is_observed() {
        let gate: &'static CallbackGate =
            Box::leak(Box::new(CallbackGate::new(ExcelCallbackGate::Open)));
        let first = gate.enter().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let suppressed = match gate.enter() {
                Ok(_) => panic!("terminal gate unexpectedly admitted a callback"),
                Err(suppressed) => suppressed,
            };
            entered_tx.send(suppressed).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(entered_rx.recv_timeout(Duration::from_millis(20)).is_err());
        first.observe(ExcelCallbackStatus::Abort);
        drop(first);
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            CallbackGateSuppressed {
                status: ExcelCallbackStatus::Abort,
            }
        );
        worker.join().unwrap();
        assert_eq!(gate.blocked_status(), Some(ExcelCallbackStatus::Abort));
    }
}
