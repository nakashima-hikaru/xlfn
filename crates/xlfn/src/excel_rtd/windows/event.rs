use super::update_event::ComApartmentGuard;
use crate::XllError;
use crate::excel_rtd::drain_wait::{DrainWork, UnstartedDrain, WorkerJoined};
use crate::win32::{
    COWAIT_DISPATCH_CALLS, CloseHandle, CoWaitForMultipleHandles, CreateEventW, DuplicateHandle,
    E_UNEXPECTED, GetCurrentProcess, GetCurrentThread, GetLastError, HANDLE, INFINITE, ResetEvent,
    SYNCHRONIZE, SetEvent, WAIT_FAILED, WAIT_OBJECT_0, WaitForSingleObject,
};
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::OnceLock;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DrainPreparationStage {
    Apartment,
    Event,
}

#[cfg(test)]
std::thread_local! {
    static DRAIN_PREPARATION_FAULT: std::cell::Cell<Option<DrainPreparationStage>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn fail_next_drain_preparation(stage: DrainPreparationStage) {
    DRAIN_PREPARATION_FAULT.with(|fault| assert!(fault.replace(Some(stage)).is_none()));
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

struct DispatchApartment(ComApartmentGuard);

impl DispatchApartment {
    fn enter() -> Result<Self, XllError> {
        #[cfg(test)]
        if preparation_fails(DrainPreparationStage::Apartment) {
            return Err(injected_preparation_error(DrainPreparationStage::Apartment));
        }
        // Preserve an existing STA. A thread without COM initialization enters
        // an MTA, so other Windows callers retain ordinary blocking behavior.
        ComApartmentGuard::enter()
            .map(Self)
            .map_err(|code| XllError::Native {
                code,
                message: "failed to initialize COM drain apartment".into(),
            })
    }

    fn prepare<W>(
        self,
        drain: UnstartedDrain<W>,
    ) -> Result<DispatchPrepared<W>, (XllError, UnstartedDrain<W>)> {
        #[cfg(test)]
        if preparation_fails(DrainPreparationStage::Event) {
            return Err((
                injected_preparation_error(DrainPreparationStage::Event),
                drain,
            ));
        }
        let completion = match ManualResetEvent::new(false) {
            Ok(event) => event,
            Err(error) => {
                return Err((
                    XllError::Native {
                        code: error.code as i32,
                        message: "failed to create COM drain completion event".into(),
                    },
                    drain,
                ));
            }
        };
        Ok(DispatchPrepared {
            drain,
            completion,
            _apartment: self.0,
        })
    }
}

/// Owns the untransferred work and both resources needed to pump its caller's
/// STA. The event is closed before the caller's apartment guard is released.
pub(crate) struct DispatchPrepared<W> {
    drain: UnstartedDrain<W>,
    completion: ManualResetEvent,
    _apartment: ComApartmentGuard,
}

/// Prepares an STA-dispatchable drain without transferring the work. Both
/// apartment and event preparation errors return the original owner.
pub(crate) fn drain_with_com_dispatch<W: DrainWork>(
    drain: UnstartedDrain<W>,
) -> Result<DispatchPrepared<W>, (XllError, UnstartedDrain<W>)> {
    match DispatchApartment::enter() {
        Ok(apartment) => apartment.prepare(drain),
        Err(error) => Err((error, drain)),
    }
}

impl<W: DrainWork> DispatchPrepared<W> {
    /// Starts the scoped worker and pumps COM until it is safe to join. A
    /// startup refusal returns the untransferred owner. Once startup succeeds,
    /// the caller keeps both COM resources alive through the worker's join.
    pub(crate) fn run(self) -> Result<WorkerJoined<W::Output>, (XllError, UnstartedDrain<W>)> {
        let completion = &self.completion;
        let terminated = OnceLock::new();
        std::thread::scope(|scope| {
            let publish_termination = &terminated;
            let transferred = self
                .drain
                .spawn(
                    scope,
                    std::thread::Builder::new().name("xlfn-sta-drain".into()),
                    move || {
                        publish_termination
                            .set(duplicate_current_thread())
                            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
                        completion
                            .set()
                            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
                    },
                )
                .map_err(|(error, drain)| {
                    (
                        XllError::Native {
                            code: error.raw_os_error().unwrap_or(0),
                            message: format!("failed to start COM drain worker: {error}"),
                        },
                        drain,
                    )
                })?;
            completion
                .wait_with_com_pumping()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
            // The completion callback runs before the helper's TLS and OS
            // exit hooks. Keep dispatching through their synchronous COM calls.
            let helper = terminated
                .get()
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            wait_with_com_pumping(helper.as_handle())
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
            Ok(transferred.join())
        })
    }
}

fn duplicate_current_thread() -> OwnedHandle {
    let mut raw = ptr::null_mut();
    // SAFETY: both process pseudo-handles designate this process; the thread
    // pseudo-handle designates this helper. The output points to live storage.
    let success = unsafe {
        let process = GetCurrentProcess();
        DuplicateHandle(
            process,
            GetCurrentThread(),
            process,
            &mut raw,
            SYNCHRONIZE as u32,
            0,
            0,
        )
    };
    if success == 0 || raw.is_null() {
        xlfn_kernel::invariant::fail_stop();
    }
    // SAFETY: successful duplication issued a real handle uniquely owned here.
    unsafe { OwnedHandle::from_raw_handle(raw) }
}

fn wait_with_com_pumping(handle: BorrowedHandle<'_>) -> Result<(), i32> {
    let raw = handle.as_raw_handle();
    let mut index = u32::MAX;
    // SAFETY: the borrow keeps the handle live; one readable HANDLE is
    // supplied and index is writable. A classic STA dispatches incoming COM
    // calls during CoWait; COWAIT_DISPATCH_CALLS also enables them for an ASTA.
    // Omit COWAIT_DISPATCH_WINDOW_MESSAGES to avoid broad window dispatch.
    let status =
        unsafe { CoWaitForMultipleHandles(COWAIT_DISPATCH_CALLS, INFINITE, 1, &raw, &mut index) };

    if status < 0 {
        Err(status)
    } else if index != 0 {
        Err(E_UNEXPECTED)
    } else {
        Ok(())
    }
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
        // SAFETY: this event uniquely owns the live handle for the borrow.
        wait_with_com_pumping(unsafe { BorrowedHandle::borrow_raw(self.raw()) })
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
    use crate::excel_rtd::drain_wait::fail_next_drain_spawn;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static_assertions::assert_not_impl_any!(DispatchPrepared<fn()>: Send, Sync);

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
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let owner = DropObserved(Arc::clone(&drops));
            let observed_calls = Arc::clone(&calls);
            fail_next_drain_preparation(stage);
            let result = drain_with_com_dispatch(UnstartedDrain::new(move || {
                observed_calls.fetch_add(1, Ordering::AcqRel);
                drop(owner);
            }));
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
    fn refused_spawn_returns_the_work_for_a_later_dispatch() {
        let calls = AtomicUsize::new(0);
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = DropObserved(Arc::clone(&drops));
        let calls_in_worker = &calls;
        let ready = drain_with_com_dispatch(UnstartedDrain::new(move || {
            calls_in_worker.fetch_add(1, Ordering::AcqRel);
            drop(owner);
            17
        }))
        .unwrap_or_else(|_| panic!("preparing the apartment and event must succeed"));

        fail_next_drain_spawn();
        let Err((error, unstarted)) = ready.run() else {
            panic!("injected spawn failure started a worker");
        };
        assert!(matches!(error, XllError::Native { .. }));
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert_eq!(drops.load(Ordering::Acquire), 0);

        let joined = drain_with_com_dispatch(unstarted)
            .and_then(|ready| ready.run())
            .unwrap_or_else(|_| panic!("retry must join the worker"));
        assert_eq!(joined.into_output(), 17);
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn preparing_does_not_transfer_work() {
        let calls = AtomicUsize::new(0);
        let ready = drain_with_com_dispatch(UnstartedDrain::new(|| {
            calls.fetch_add(1, Ordering::AcqRel);
        }))
        .unwrap_or_else(|_| panic!("preparing the apartment and event must succeed"));
        assert_eq!(calls.load(Ordering::Acquire), 0);
        drop(ready);
        assert_eq!(calls.load(Ordering::Acquire), 0);
    }
}
