use crate::win32::{
    COWAIT_DISPATCH_CALLS, CloseHandle, CoWaitForMultipleHandles, CreateEventW, E_UNEXPECTED,
    GetLastError, HANDLE, INFINITE, ResetEvent, SetEvent, WAIT_FAILED, WAIT_OBJECT_0,
    WaitForSingleObject,
};
use std::ptr;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DrainPreparationStage {
    Apartment,
    Event,
    Spawn,
}

#[cfg(test)]
std::thread_local! {
    static DRAIN_PREPARATION_FAULT: std::cell::Cell<Option<DrainPreparationStage>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn preparation_fails(stage: DrainPreparationStage) -> bool {
    DRAIN_PREPARATION_FAULT.with(|fault| {
        if fault.get() == Some(stage) {
            fault.set(None);
            true
        } else {
            false
        }
    })
}

#[cfg(test)]
fn injected_preparation_error(stage: DrainPreparationStage) -> crate::XllError {
    crate::XllError::Native {
        code: E_UNEXPECTED,
        message: format!("injected COM drain preparation failure: {stage:?}"),
    }
}

/// Keeps the caller's STA dispatchable while the existing drain protocol
/// blocks. The scoped worker only waits for the gate; it runs no application
/// callbacks and cannot outlive either the gate or its completion event.
/// Preparation failures return the unexecuted closure with its affine owners.
/// Failures after worker startup remain fail-stop because completion cannot
/// then be certified and the worker may still borrow closing resources.
pub(crate) fn drain_with_com_dispatch<F: FnOnce() + Send>(
    wait: F,
) -> Result<(), (crate::XllError, F)> {
    #[cfg(test)]
    if preparation_fails(DrainPreparationStage::Apartment) {
        return Err((
            injected_preparation_error(DrainPreparationStage::Apartment),
            wait,
        ));
    }
    // Preserve an existing STA. A thread without COM initialization enters an
    // MTA for this wait, so ordinary Windows callers retain blocking behavior.
    let _apartment = match super::update_event::ComApartmentGuard::enter() {
        Ok(apartment) => apartment,
        Err(code) => {
            return Err((
                crate::XllError::Native {
                    code,
                    message: "failed to initialize COM drain apartment".into(),
                },
                wait,
            ));
        }
    };
    #[cfg(test)]
    if preparation_fails(DrainPreparationStage::Event) {
        return Err((
            injected_preparation_error(DrainPreparationStage::Event),
            wait,
        ));
    }
    let completed = match ManualResetEvent::new(false) {
        Ok(event) => event,
        Err(error) => {
            return Err((
                crate::XllError::Native {
                    code: error.code as i32,
                    message: "failed to create COM drain completion event".into(),
                },
                wait,
            ));
        }
    };
    // Borrow the closure until startup succeeds. spawn_scoped drops its
    // captured arguments on failure; returning the unused closure keeps any
    // join handle or other affine owner available to the closing transaction.
    let waiting = std::sync::Mutex::new(Some(wait));
    std::thread::scope(|scope| {
        let completion = &completed;
        let waiting = &waiting;
        let worker_fn = move || {
            let wait = waiting
                .lock()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop())
                .take()
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            // Signal on panic too; otherwise the caller could wait forever
            // for a failed worker. No application-owned drop follows this
            // signal: the wait closure has already been consumed.
            let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(wait));
            completion
                .set()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
            result
        };
        #[cfg(test)]
        let injected_spawn_failure = preparation_fails(DrainPreparationStage::Spawn);
        #[cfg(not(test))]
        let injected_spawn_failure = false;
        let spawned = if injected_spawn_failure {
            Err(std::io::Error::other(
                "injected COM drain worker spawn failure",
            ))
        } else {
            std::thread::Builder::new()
                .name("xlfn-sta-drain".into())
                .spawn_scoped(scope, worker_fn)
        };
        let worker = match spawned {
            Ok(worker) => worker,
            Err(error) => {
                let wait = waiting
                    .lock()
                    .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop())
                    .take()
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
                return Err((
                    crate::XllError::Native {
                        code: error.raw_os_error().unwrap_or(0),
                        message: format!("failed to start COM drain worker: {error}"),
                    },
                    wait,
                ));
            }
        };
        completed
            .wait_with_com_pumping()
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        let result = crate::panic_boundary::contain_panic(worker.join())
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        if result.is_err() {
            // No drain certificate may escape a failed wait, and the scoped
            // gate/event owner remains live until this fail-stop.
            xlfn_kernel::invariant::fail_stop();
        }
        Ok(())
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Win32EventError {
    pub(super) operation: &'static str,
    pub(super) code: u32,
}

pub(super) struct ManualResetEvent {
    // The generated HANDLE alias is a pointer and is therefore not Send/Sync.
    // The underlying unnamed kernel event is process-wide and safely waitable
    // from any thread, so retain its non-zero bit pattern in a plain integer.
    handle: usize,
}

impl ManualResetEvent {
    pub(super) fn new(initial_state: bool) -> Result<Self, Win32EventError> {
        // SAFETY: null security attributes select the default descriptor, both
        // BOOL values are valid, and a null name requests an unnamed event.
        let handle = unsafe { CreateEventW(ptr::null(), 1, i32::from(initial_state), ptr::null()) };
        if handle.is_null() {
            // SAFETY: CreateEventW just reported failure on this thread.
            let code = unsafe { GetLastError() };
            return Err(Win32EventError {
                operation: "CreateEventW",
                code,
            });
        }

        Ok(Self {
            handle: handle as usize,
        })
    }

    pub(super) fn raw(&self) -> HANDLE {
        self.handle as HANDLE
    }

    pub(super) fn reset(&self) -> Result<(), Win32EventError> {
        // SAFETY: this RAII object owns a live manual-reset event handle.
        if unsafe { ResetEvent(self.raw()) } == 0 {
            // SAFETY: ResetEvent just reported failure on this thread.
            let code = unsafe { GetLastError() };
            Err(Win32EventError {
                operation: "ResetEvent",
                code,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn set(&self) -> Result<(), Win32EventError> {
        // SAFETY: this RAII object owns a live manual-reset event handle.
        if unsafe { SetEvent(self.raw()) } == 0 {
            // SAFETY: SetEvent just reported failure on this thread.
            let code = unsafe { GetLastError() };
            Err(Win32EventError {
                operation: "SetEvent",
                code,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn wait_with_com_pumping(&self) -> Result<(), i32> {
        let handle = self.raw();
        let mut index = u32::MAX;
        // SAFETY: `handle` remains live for this call, one readable HANDLE is
        // supplied, and `index` is writable. A classic STA dispatches incoming
        // COM calls during CoWait; COWAIT_DISPATCH_CALLS additionally enables
        // that behavior for an ASTA and is ignored by other apartment types.
        // Deliberately omit COWAIT_DISPATCH_WINDOW_MESSAGES so teardown cannot
        // run an arbitrary Windows message loop.
        let status = unsafe {
            CoWaitForMultipleHandles(COWAIT_DISPATCH_CALLS, INFINITE, 1, &handle, &mut index)
        };

        if status < 0 {
            Err(status)
        } else if index != 0 {
            Err(E_UNEXPECTED)
        } else {
            Ok(())
        }
    }

    pub(super) fn wait_blocking(&self) -> Result<(), i32> {
        // SAFETY: this RAII object owns a live event handle for the complete
        // wait. A coordinator thread has no STA work to dispatch.
        let status = unsafe { WaitForSingleObject(self.raw(), INFINITE) };
        if status == WAIT_OBJECT_0 as u32 {
            Ok(())
        } else if status == WAIT_FAILED {
            // SAFETY: WaitForSingleObject just reported failure on this thread.
            Err(unsafe { GetLastError() } as i32)
        } else {
            Err(E_UNEXPECTED)
        }
    }
}

impl Drop for ManualResetEvent {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the non-null handle returned by
        // CreateEventW and closes it exactly once after all barrier borrows end.
        let closed = unsafe { CloseHandle(self.raw()) };
        debug_assert_ne!(closed, 0, "CloseHandle failed for RTD barrier event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DropObserved(Arc<AtomicUsize>);

    impl Drop for DropObserved {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn preparation_failures_return_the_unused_wait_owner() {
        for stage in [
            DrainPreparationStage::Apartment,
            DrainPreparationStage::Event,
            DrainPreparationStage::Spawn,
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let owner = DropObserved(Arc::clone(&drops));
            let observed_calls = Arc::clone(&calls);
            DRAIN_PREPARATION_FAULT.with(|fault| assert!(fault.replace(Some(stage)).is_none()));
            let result = drain_with_com_dispatch(move || {
                observed_calls.fetch_add(1, Ordering::AcqRel);
                drop(owner);
            });
            let Err((error, unused_wait)) = result else {
                panic!("injected {stage:?} preparation failure succeeded");
            };
            assert!(matches!(error, crate::XllError::Native { .. }));
            assert_eq!(calls.load(Ordering::Acquire), 0);
            assert_eq!(drops.load(Ordering::Acquire), 0);
            drop(unused_wait);
            assert_eq!(drops.load(Ordering::Acquire), 1);
            DRAIN_PREPARATION_FAULT.with(|fault| assert!(fault.get().is_none()));
        }
    }

    #[test]
    fn prepared_drain_executes_the_wait_once() {
        let calls = AtomicUsize::new(0);
        assert!(
            drain_with_com_dispatch(|| {
                calls.fetch_add(1, Ordering::AcqRel);
            })
            .is_ok()
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }
}
