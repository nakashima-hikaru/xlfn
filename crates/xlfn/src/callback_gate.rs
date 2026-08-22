use crate::return_value::ExcelCallbackStatus;
use parking_lot::{ReentrantMutex, ReentrantMutexGuard};
use std::cell::{Cell, RefCell};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use xlfn_sys::XLRET_FAILED;

#[cfg(test)]
static GATE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
static GATES: std::sync::LazyLock<parking_lot::Mutex<HashMap<u64, &'static CallbackGate>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallbackGateLifecycle {
    Open,
    Closed,
}

#[derive(Debug)]
pub(crate) struct CallbackGateState {
    pub(crate) lifecycle: CallbackGateLifecycle,
    pub(crate) abort_scopes: usize,
    pub(crate) uncalced_scopes: usize,
}

impl CallbackGateState {
    const fn new(lifecycle: CallbackGateLifecycle) -> Self {
        Self {
            lifecycle,
            abort_scopes: 0,
            uncalced_scopes: 0,
        }
    }
}

pub(crate) struct CallbackInvocationToken {
    terminal: Cell<Option<ExcelCallbackStatus>>,
    gate_id: Cell<Option<u64>>,
}

impl CallbackInvocationToken {
    pub(crate) fn new() -> Self {
        Self {
            terminal: Cell::new(None),
            gate_id: Cell::new(None),
        }
    }

    pub(crate) fn finish(&self) {
        finish_invocation(self);
    }
}

impl Drop for CallbackInvocationToken {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CallbackGateSuppressed {
    pub(crate) status: ExcelCallbackStatus,
}

pub(crate) struct CallbackGate {
    id: u64,
    state: ReentrantMutex<RefCell<CallbackGateState>>,
}

impl CallbackGate {
    pub(crate) const fn new(initial: CallbackGateLifecycle) -> Self {
        Self {
            id: 0,
            state: ReentrantMutex::new(RefCell::new(CallbackGateState::new(initial))),
        }
    }

    #[cfg(test)]
    fn new_test(initial: CallbackGateLifecycle) -> &'static Self {
        let id = GATE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let gate = Box::leak(Box::new(Self {
            id,
            state: ReentrantMutex::new(RefCell::new(CallbackGateState::new(initial))),
        }));
        GATES.lock().insert(id, gate);
        gate
    }

    pub(crate) fn reset(&self) {
        self.state.lock().borrow_mut().lifecycle = CallbackGateLifecycle::Open;
    }

    pub(crate) fn close(&self) {
        self.state.lock().borrow_mut().lifecycle = CallbackGateLifecycle::Closed;
    }

    fn enter_callback<'a>(
        &'a self,
        invocation: &'a CallbackInvocationToken,
    ) -> Result<CallbackGatePermit<'a>, CallbackGateSuppressed> {
        let lock = self.state.lock();
        let state = lock.borrow();
        match state.lifecycle {
            CallbackGateLifecycle::Closed => Err(CallbackGateSuppressed {
                status: ExcelCallbackStatus::Failed(XLRET_FAILED),
            }),
            CallbackGateLifecycle::Open => {
                if state.abort_scopes != 0 {
                    Err(CallbackGateSuppressed {
                        status: ExcelCallbackStatus::Abort,
                    })
                } else if state.uncalced_scopes != 0 {
                    Err(CallbackGateSuppressed {
                        status: ExcelCallbackStatus::Uncalced,
                    })
                } else {
                    drop(state);
                    Ok(CallbackGatePermit {
                        gate: self,
                        _guard: lock,
                        invocation: Some(invocation),
                    })
                }
            }
        }
    }

    fn enter_cleanup<'a>(
        &'a self,
        invocation: Option<&'a CallbackInvocationToken>,
    ) -> Result<CallbackGatePermit<'a>, CallbackGateSuppressed> {
        let lock = self.state.lock();
        let state = lock.borrow();
        match state.lifecycle {
            CallbackGateLifecycle::Closed => Err(CallbackGateSuppressed {
                status: ExcelCallbackStatus::Failed(XLRET_FAILED),
            }),
            CallbackGateLifecycle::Open => {
                drop(state);
                Ok(CallbackGatePermit {
                    gate: self,
                    _guard: lock,
                    invocation,
                })
            }
        }
    }

    #[cfg(test)]
    fn blocked_status(&self) -> Option<ExcelCallbackStatus> {
        callback_blocked_status(&self.state.lock())
    }
}

pub(crate) struct CallbackGatePermit<'a> {
    gate: &'a CallbackGate,
    _guard: ReentrantMutexGuard<'a, RefCell<CallbackGateState>>,
    invocation: Option<&'a CallbackInvocationToken>,
}

impl CallbackGatePermit<'_> {
    pub(crate) fn observe(&self, status: ExcelCallbackStatus) {
        if let Some(invocation) = self.invocation {
            observe_terminal(self.gate, invocation, status);
        }
    }
}

#[cfg(test)]
fn callback_blocked_status(state: &RefCell<CallbackGateState>) -> Option<ExcelCallbackStatus> {
    let gate = state.borrow();
    match gate.lifecycle {
        CallbackGateLifecycle::Closed => Some(ExcelCallbackStatus::Failed(XLRET_FAILED)),
        CallbackGateLifecycle::Open => {
            if gate.abort_scopes != 0 {
                Some(ExcelCallbackStatus::Abort)
            } else if gate.uncalced_scopes != 0 {
                Some(ExcelCallbackStatus::Uncalced)
            } else {
                None
            }
        }
    }
}

fn observe_terminal(
    gate: &CallbackGate,
    invocation: &CallbackInvocationToken,
    status: ExcelCallbackStatus,
) {
    if !status.is_terminal() || invocation.terminal.get().is_some() {
        return;
    }
    invocation.gate_id.set(Some(gate.id));
    invocation.terminal.set(Some(status));
    let lock = gate.state.lock();
    let mut state = lock.borrow_mut();
    match status {
        ExcelCallbackStatus::Abort => {
            state.abort_scopes += 1;
        }
        ExcelCallbackStatus::Uncalced => {
            state.uncalced_scopes += 1;
        }
        _ => unreachable!(),
    }
}

fn finish_invocation(invocation: &CallbackInvocationToken) {
    let Some(status) = invocation.terminal.take() else {
        return;
    };
    let gate_id = invocation.gate_id.take().unwrap_or(0);
    #[cfg(test)]
    if gate_id != 0 {
        let map = GATES.lock();
        if let Some(gate) = map.get(&gate_id) {
            decrement_scope(&gate.state, status);
        }
        return;
    }
    #[cfg(not(test))]
    let _ = gate_id;
    decrement_scope(
        &crate::module_runtime::global().callback_gate().state,
        status,
    );
}

fn decrement_scope(
    state: &ReentrantMutex<RefCell<CallbackGateState>>,
    status: ExcelCallbackStatus,
) {
    let lock = state.lock();
    let mut gate = lock.borrow_mut();
    match status {
        ExcelCallbackStatus::Abort => {
            gate.abort_scopes = gate
                .abort_scopes
                .checked_sub(1)
                .expect("balanced terminal callback scope");
        }
        ExcelCallbackStatus::Uncalced => {
            gate.uncalced_scopes = gate
                .uncalced_scopes
                .checked_sub(1)
                .expect("balanced terminal callback scope");
        }
        _ => unreachable!(),
    }
}

pub(crate) fn enter_callback<'a>(
    invocation: &'a CallbackInvocationToken,
) -> Result<CallbackGatePermit<'a>, CallbackGateSuppressed> {
    crate::module_runtime::global()
        .callback_gate()
        .enter_callback(invocation)
}

pub(crate) fn enter_cleanup<'a>(
    invocation: Option<&'a CallbackInvocationToken>,
) -> Result<CallbackGatePermit<'a>, CallbackGateSuppressed> {
    crate::module_runtime::global()
        .callback_gate()
        .enter_cleanup(invocation)
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
        crate::module_runtime::global().reset_callbacks();
        let token = CallbackInvocationToken::new();
        assert!(enter_callback(&token).is_ok());
        crate::module_runtime::global().close_callbacks();
        assert!(matches!(
            enter_callback(&token),
            Err(CallbackGateSuppressed { .. })
        ));
    }

    #[test]
    fn terminal_status_is_module_wide_only_while_owner_is_active() {
        let gate = CallbackGate::new_test(CallbackGateLifecycle::Closed);
        gate.reset();

        let token_a = CallbackInvocationToken::new();
        let token_b = CallbackInvocationToken::new();

        let permit_a = gate.enter_callback(&token_a).unwrap();
        permit_a.observe(ExcelCallbackStatus::from_raw(XLRET_ABORT));
        drop(permit_a);

        // B's normal callback is suppressed while A is active
        let suppressed = match gate.enter_callback(&token_b) {
            Ok(_) => panic!("terminal gate unexpectedly admitted a callback"),
            Err(suppressed) => suppressed,
        };
        assert_eq!(
            suppressed,
            CallbackGateSuppressed {
                status: ExcelCallbackStatus::Abort,
            }
        );

        // While A is active, cleanup callback STILL succeeds!
        assert!(gate.enter_cleanup(Some(&token_b)).is_ok());

        // When A finishes (token_a dropped), B's normal callback becomes allowed again
        drop(token_a);
        assert!(gate.enter_callback(&token_b).is_ok());

        // If both A and B enter terminal state:
        let token_a2 = CallbackInvocationToken::new();
        let token_b2 = CallbackInvocationToken::new();
        let permit_a2 = gate.enter_callback(&token_a2).unwrap();
        permit_a2.observe(ExcelCallbackStatus::from_raw(XLRET_ABORT));
        drop(permit_a2);

        let permit_b2 = gate.enter_cleanup(Some(&token_b2)).unwrap();
        permit_b2.observe(ExcelCallbackStatus::from_raw(XLRET_UNCALCED));
        drop(permit_b2);

        // A2 finishes
        drop(token_a2);
        // B2 is still active with Uncalced, so normal callback is still suppressed
        assert_eq!(gate.blocked_status(), Some(ExcelCallbackStatus::Uncalced));
        let token_c = CallbackInvocationToken::new();
        assert!(gate.enter_callback(&token_c).is_err());

        // B2 finishes
        drop(token_b2);
        assert_eq!(gate.blocked_status(), None);
        assert!(gate.enter_callback(&token_c).is_ok());

        // Closed is final and won't reopen on token drop
        gate.close();
        assert_eq!(
            gate.blocked_status(),
            Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
        );
    }

    #[test]
    fn permit_serializes_callbacks_until_terminal_status_is_observed() {
        let gate = CallbackGate::new_test(CallbackGateLifecycle::Open);
        let token_a = CallbackInvocationToken::new();
        let first = gate.enter_callback(&token_a).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let token_b = CallbackInvocationToken::new();
            started_tx.send(()).unwrap();
            let suppressed = match gate.enter_callback(&token_b) {
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
