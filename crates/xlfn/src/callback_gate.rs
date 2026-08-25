use crate::return_value::ExcelCallbackStatus;
use parking_lot::Mutex;
use xlfn_sys::XLRET_FAILED;

/// The module-wide callback lifecycle is independent from the state of any
/// one Excel invocation. `Closing` rejects new callbacks but still permits
/// cleanup for callbacks that were admitted before the close began.
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

    fn enter(
        &'static self,
        cleanup: bool,
    ) -> Result<ModuleCallbackPermit, CallbackAdmissionSuppressed> {
        let mut state = self.state.lock();
        let allowed = match state.lifecycle {
            ModuleCallbackLifecycle::Open => true,
            ModuleCallbackLifecycle::Closing => cleanup,
            ModuleCallbackLifecycle::Closed => false,
        };
        if !allowed {
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

/// One short-lived module admission. The token is dropped immediately after
/// the raw Excel call returns, so no synchronization primitive is held across
/// the host call.
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
    crate::module_runtime::global()
        .callback_admission()
        .enter(false)
}

pub(crate) fn enter_cleanup() -> Result<ModuleCallbackPermit, CallbackAdmissionSuppressed> {
    crate::module_runtime::global()
        .callback_admission()
        .enter(true)
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
        let first = gate.enter(false).unwrap();
        let second = gate.enter(false).unwrap();
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
        let first = gate.enter(false).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let second = gate
                .enter(false)
                .expect("callbacks are admitted concurrently");
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
    fn cleanup_is_allowed_until_the_last_admitted_callback_finishes() {
        let gate = Box::leak(Box::new(ModuleCallbackAdmission::new(
            ModuleCallbackLifecycle::Open,
        )));
        let callback = gate.enter(false).unwrap();
        gate.close();
        assert!(gate.enter(false).is_err());
        let cleanup = gate.enter(true).unwrap();
        drop(cleanup);
        assert!(gate.blocked_status().is_some());
        drop(callback);
        assert_eq!(
            gate.blocked_status(),
            Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
        );
    }
}
