use crate::return_abi::ExcelCallbackStatus;
use crate::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use xlfn_kernel::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate, StripedOwnedDrainPermit};
use xlfn_sys::XLRET_FAILED;

/// Module closure rejects new callbacks before sealing the striped counters.
/// Already admitted results retain their permit through the final `xlFree`.
/// Closing is observed as Closed once all retained permits have drained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ModuleCallbackLifecycle {
    Open,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CallbackAdmissionSuppressed {
    pub(crate) status: ExcelCallbackStatus,
}

/// Module-wide admission for calls into Excel's callback ABI.
///
/// Ordinary callbacks acquire and release only the calling thread's counter.
/// The transition mutex serializes close/reopen, never callback execution or
/// cleanup. Invocation-local terminal statuses remain in HostCallbackSession.
/// The module owns this gate for process lifetime; the kernel gate owns the
/// admission/release ordering and the counter overflow checks.
pub(crate) struct ModuleCallbackAdmission {
    phase: AtomicU8,
    callbacks: StripedDrainGate<DEFAULT_STRIPE_COUNT>,
    transition: Mutex<()>,
}

impl ModuleCallbackAdmission {
    pub(crate) const fn new(initial: ModuleCallbackLifecycle) -> Self {
        Self {
            phase: AtomicU8::new(initial as u8),
            callbacks: match initial {
                ModuleCallbackLifecycle::Open => StripedDrainGate::new_open(),
                ModuleCallbackLifecycle::Closing | ModuleCallbackLifecycle::Closed => {
                    StripedDrainGate::new_sealed()
                }
            },
            transition: Mutex::new(()),
        }
    }

    pub(crate) fn reset(&self) {
        let _transition = self.transition.lock();
        let active = self.callbacks.active();
        if active != 0 {
            tracing::error!(
                active,
                "callback admission reopened while callbacks are still active"
            );
            std::process::abort();
        }
        // A closed gate stays unavailable until every stripe has reopened.
        // Repeating reset on an already open, idle gate requires no counter
        // reset and preserves reservations that start after the active check.
        if self.callbacks.is_sealed() {
            self.callbacks
                .reopen()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        }
        self.phase
            .store(ModuleCallbackLifecycle::Open as u8, Ordering::Release);
    }

    pub(crate) fn close(&self) {
        let _transition = self.transition.lock();
        // Publish rejection first: no late entrant may use a stripe that the
        // seal loop has not reached yet. A racing reservation is rechecked by
        // enter and either retained as an admitted callback or released there.
        self.phase
            .store(ModuleCallbackLifecycle::Closing as u8, Ordering::Release);
        self.callbacks.seal();
    }

    #[inline]
    fn enter(&'static self) -> Result<ModuleCallbackPermit, CallbackAdmissionSuppressed> {
        let suppressed = || CallbackAdmissionSuppressed {
            status: ExcelCallbackStatus::Failed(XLRET_FAILED),
        };
        if self.phase.load(Ordering::Acquire) != ModuleCallbackLifecycle::Open as u8 {
            return Err(suppressed());
        }
        let permit = self
            .callbacks
            .try_enter_owned_current()
            .map_err(|_| suppressed())?;
        if self.phase.load(Ordering::Acquire) != ModuleCallbackLifecycle::Open as u8 {
            drop(permit);
            return Err(suppressed());
        }
        Ok(ModuleCallbackPermit { _permit: permit })
    }

    #[cfg(test)]
    fn lifecycle(&self) -> ModuleCallbackLifecycle {
        if self.phase.load(Ordering::Acquire) == ModuleCallbackLifecycle::Open as u8 {
            ModuleCallbackLifecycle::Open
        } else if self.callbacks.active() == 0 {
            ModuleCallbackLifecycle::Closed
        } else {
            ModuleCallbackLifecycle::Closing
        }
    }

    #[cfg(test)]
    fn blocked_status(&self) -> Option<ExcelCallbackStatus> {
        (self.phase.load(Ordering::Acquire) != ModuleCallbackLifecycle::Open as u8)
            .then_some(ExcelCallbackStatus::Failed(XLRET_FAILED))
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        self.callbacks.active()
    }
}

/// One callback reservation, retained until its host result is released.
/// The owned striped permit can be transferred to another cleanup thread;
/// release always uses its original stripe rather than the dropping thread.
pub(crate) struct ModuleCallbackPermit {
    _permit: StripedOwnedDrainPermit<DEFAULT_STRIPE_COUNT>,
}

#[inline]
pub(crate) fn enter_callback() -> Result<ModuleCallbackPermit, CallbackAdmissionSuppressed> {
    crate::module_runtime::global().callback_admission().enter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};
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

    #[test]
    fn miri_callback_permit_releases_the_acquiring_stripe_on_another_thread() {
        static GATE: ModuleCallbackAdmission =
            ModuleCallbackAdmission::new(ModuleCallbackLifecycle::Open);
        let gate = &GATE;
        let permit = gate.enter().unwrap();
        gate.close();
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closing);
        std::thread::spawn(move || drop(permit)).join().unwrap();
        assert_eq!(gate.active(), 0);
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closed);
        gate.reset();
        let next_epoch = gate.enter().unwrap();
        assert_eq!(gate.active(), 1);
        drop(next_epoch);
    }

    #[test]
    fn close_rejects_all_workers_while_their_callback_results_are_retained() {
        const WORKERS: usize = 8;
        let gate: &'static ModuleCallbackAdmission = Box::leak(Box::new(
            ModuleCallbackAdmission::new(ModuleCallbackLifecycle::Open),
        ));
        let acquired = Arc::new(Barrier::new(WORKERS + 1));
        let closed = Arc::new(Barrier::new(WORKERS + 1));
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                let acquired = Arc::clone(&acquired);
                let closed = Arc::clone(&closed);
                std::thread::spawn(move || {
                    let result_permit = gate.enter().unwrap();
                    acquired.wait();
                    closed.wait();
                    assert!(gate.enter().is_err());
                    drop(result_permit);
                })
            })
            .collect();
        acquired.wait();
        assert_eq!(gate.active(), WORKERS);
        // Close is nonblocking even when invoked from an admitted callback.
        gate.close();
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closing);
        closed.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closed);
        gate.reset();
        assert!(gate.enter().is_ok());
    }

    #[test]
    fn miri_module_closure_rejects_an_unsealed_callback_stripe() {
        static GATE: ModuleCallbackAdmission =
            ModuleCallbackAdmission::new(ModuleCallbackLifecycle::Open);
        let gate = &GATE;
        // Exercise the close handoff: the phase is closed to new calls before
        // the counter seal loop has reached the current thread's stripe.
        gate.phase
            .store(ModuleCallbackLifecycle::Closing as u8, Ordering::Release);
        assert!(!gate.callbacks.is_sealed());
        assert!(gate.enter().is_err());
        gate.close();
        gate.reset();
        assert!(gate.enter().is_ok());
    }
}
