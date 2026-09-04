use crate::return_abi::ExcelCallbackStatus;
use parking_lot::Mutex;
use xlfn_sys::XLRET_FAILED;

/// The module-wide callback lifecycle is independent from the state of any
/// one Excel invocation. `Closing` rejects new callbacks while outstanding
/// results hold their `ModuleCallbackPermit` until cleanup completes. Once all
/// admitted callbacks and their results are released, the gate transitions to
/// `Closed`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModuleCallbackLifecycle {
    Open,
    Closing,
    Closed,
}

#[derive(Debug)]
struct ModuleCallbackState {
    lifecycle: ModuleCallbackLifecycle,
    active: usize,
}

impl ModuleCallbackState {
    const fn new(lifecycle: ModuleCallbackLifecycle) -> Self {
        Self {
            lifecycle,
            active: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CallbackAdmissionSuppressed {
    pub(crate) status: ExcelCallbackStatus,
}

/// Module-wide admission for calls into Excel's callback ABI.
///
/// Admission is deliberately short-lived and never holds the state mutex
/// while Excel is executing. Concurrent callbacks are therefore admitted in
/// parallel. Invocation-local terminal statuses belong to
/// `HostCallbackSession`; putting them here would incorrectly make one
/// invocation suppress unrelated callbacks in other threads.
pub(crate) struct ModuleCallbackAdmission {
    state: Mutex<ModuleCallbackState>,
}

impl ModuleCallbackAdmission {
    pub(crate) const fn new(initial: ModuleCallbackLifecycle) -> Self {
        Self {
            state: Mutex::new(ModuleCallbackState::new(initial)),
        }
    }

    pub(crate) fn reset(&self) {
        let mut state = self.state.lock();
        if state.active != 0 {
            tracing::error!(
                active = state.active,
                "callback admission reopened while callbacks are still active"
            );
            std::process::abort();
        }
        state.lifecycle = ModuleCallbackLifecycle::Open;
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock();
        match state.lifecycle {
            ModuleCallbackLifecycle::Closed => return,
            ModuleCallbackLifecycle::Open => state.lifecycle = ModuleCallbackLifecycle::Closing,
            ModuleCallbackLifecycle::Closing => {}
        }

        if state.active == 0 {
            state.lifecycle = ModuleCallbackLifecycle::Closed;
        }
    }

    fn enter(&'static self) -> Result<ModuleCallbackPermit, CallbackAdmissionSuppressed> {
        let mut state = self.state.lock();
        if state.lifecycle != ModuleCallbackLifecycle::Open {
            return Err(CallbackAdmissionSuppressed {
                status: ExcelCallbackStatus::Failed(XLRET_FAILED),
            });
        }

        state.active = state.active.checked_add(1).unwrap_or_else(|| {
            tracing::error!("module callback admission counter exhausted; fail-stopping");
            std::process::abort();
        });
        Ok(ModuleCallbackPermit { admission: self })
    }

    #[cfg(test)]
    fn lifecycle(&self) -> ModuleCallbackLifecycle {
        self.state.lock().lifecycle
    }

    #[cfg(test)]
    fn blocked_status(&self) -> Option<ExcelCallbackStatus> {
        let state = self.state.lock();
        match state.lifecycle {
            ModuleCallbackLifecycle::Open => None,
            ModuleCallbackLifecycle::Closing | ModuleCallbackLifecycle::Closed => {
                Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
            }
        }
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        self.state.lock().active
    }
}

/// Module admission capability. For short-lived operations (like async return),
/// the token is dropped after the host call returns. For callback results,
/// ownership of the permit is transferred into `ExcelCallbackValue` to hold
/// the module open until cleanup completes.
pub(crate) struct ModuleCallbackPermit {
    admission: &'static ModuleCallbackAdmission,
}

impl Drop for ModuleCallbackPermit {
    fn drop(&mut self) {
        let mut state = self.admission.state.lock();
        state.active = state.active.checked_sub(1).unwrap_or_else(|| {
            tracing::error!("module callback admission underflow; fail-stopping");
            std::process::abort();
        });
        if state.active == 0 && state.lifecycle == ModuleCallbackLifecycle::Closing {
            state.lifecycle = ModuleCallbackLifecycle::Closed;
        }
    }
}

pub(crate) fn enter_callback() -> Result<ModuleCallbackPermit, CallbackAdmissionSuppressed> {
    crate::module_runtime::global().callback_admission().enter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn runtime_transitions_update_the_module_admission() {
        let _test_guard = crate::test_callback::lock();
        crate::module_runtime::reset_callbacks_for_test();
        let permit = enter_callback().expect("open module admits callbacks");
        crate::module_runtime::close_callbacks_for_test();
        assert!(matches!(
            enter_callback(),
            Err(CallbackAdmissionSuppressed { .. })
        ));
        drop(permit);
    }

    #[test]
    fn terminal_status_is_scoped_to_the_host_session() {
        let gate: &'static ModuleCallbackAdmission = Box::leak(Box::new(
            ModuleCallbackAdmission::new(ModuleCallbackLifecycle::Open),
        ));
        let first = gate.enter().unwrap();
        let second = gate.enter().unwrap();
        assert_eq!(gate.active(), 2);
        drop(first);
        drop(second);
        assert_eq!(gate.blocked_status(), None);

        gate.close();
        assert_eq!(
            gate.blocked_status(),
            Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
        );
    }

    #[test]
    fn concurrent_callbacks_do_not_serialize_on_module_admission() {
        let gate: &'static ModuleCallbackAdmission = Box::leak(Box::new(
            ModuleCallbackAdmission::new(ModuleCallbackLifecycle::Open),
        ));
        let first = gate.enter().unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let second = gate.enter().expect("callbacks are admitted concurrently");
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(second);
        });

        assert!(entered_rx.recv_timeout(Duration::from_secs(1)).is_ok());
        assert_eq!(gate.active(), 2);
        release_tx.send(()).unwrap();
        drop(first);
        worker.join().unwrap();
        assert_eq!(gate.active(), 0);
    }

    #[test]
    fn closing_waits_for_admitted_callback_permits_before_closed() {
        let gate = Box::leak(Box::new(ModuleCallbackAdmission::new(
            ModuleCallbackLifecycle::Open,
        )));
        let callback = gate.enter().unwrap();
        gate.close();
        assert!(gate.enter().is_err());
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closing);
        assert_eq!(gate.active(), 1);
        drop(callback);
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closed);
        assert_eq!(gate.active(), 0);
        assert_eq!(
            gate.blocked_status(),
            Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
        );
    }
}
