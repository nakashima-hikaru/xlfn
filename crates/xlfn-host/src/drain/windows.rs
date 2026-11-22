use super::{DrainWork, UnstartedDrain, WorkerJoined};
use crate::com_wait::{
    ComApartmentGuard, ManualResetEvent, Win32EventError, wait_with_com_pumping,
};
use crate::win32::{DuplicateHandle, GetCurrentProcess, GetCurrentThread, INFINITE, SYNCHRONIZE};
use std::os::windows::io::{AsHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::OnceLock;
/// Operation whose failure precedes handing the work to a helper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainPreparationStage {
    Apartment,
    Event,
}

/// Native setup failure. The facade owns error shaping and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainPreparationError {
    Apartment(i32),
    Event(Win32EventError),
    Injected(DrainPreparationStage),
}

#[cfg(any(test, feature = "dispatch-test-hooks"))]
std::thread_local! {
    static DRAIN_PREPARATION_FAULT: std::cell::Cell<Option<DrainPreparationStage>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "dispatch-test-hooks"))]
pub fn fail_next_drain_preparation(stage: DrainPreparationStage) {
    DRAIN_PREPARATION_FAULT.with(|fault| assert!(fault.replace(Some(stage)).is_none()));
}

#[cfg(any(test, feature = "dispatch-test-hooks"))]
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

struct DispatchApartment(ComApartmentGuard);

impl DispatchApartment {
    fn enter() -> Result<Self, DrainPreparationError> {
        #[cfg(any(test, feature = "dispatch-test-hooks"))]
        if preparation_fails(DrainPreparationStage::Apartment) {
            return Err(DrainPreparationError::Injected(
                DrainPreparationStage::Apartment,
            ));
        }
        // Preserve an existing STA. A thread without COM initialization enters
        // an MTA, so other Windows callers retain ordinary blocking behavior.
        ComApartmentGuard::enter()
            .map(Self)
            .map_err(DrainPreparationError::Apartment)
    }

    fn prepare<W>(
        self,
        drain: UnstartedDrain<W>,
    ) -> Result<DispatchPrepared<W>, (DrainPreparationError, UnstartedDrain<W>)> {
        #[cfg(any(test, feature = "dispatch-test-hooks"))]
        if preparation_fails(DrainPreparationStage::Event) {
            return Err((
                DrainPreparationError::Injected(DrainPreparationStage::Event),
                drain,
            ));
        }
        let completion = match ManualResetEvent::new(false) {
            Ok(event) => event,
            Err(error) => {
                return Err((DrainPreparationError::Event(error), drain));
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
pub struct DispatchPrepared<W> {
    drain: UnstartedDrain<W>,
    completion: ManualResetEvent,
    _apartment: ComApartmentGuard,
}

/// Prepares an STA-dispatchable drain without transferring the work. Both
/// apartment and event preparation errors return the original owner.
pub fn prepare_com_dispatch<W: DrainWork>(
    drain: UnstartedDrain<W>,
) -> Result<DispatchPrepared<W>, (DrainPreparationError, UnstartedDrain<W>)> {
    match DispatchApartment::enter() {
        Ok(apartment) => apartment.prepare(drain),
        Err(error) => Err((error, drain)),
    }
}

impl<W: DrainWork> DispatchPrepared<W> {
    /// Starts the scoped worker and pumps COM until it is safe to join. A
    /// startup refusal returns the untransferred owner. Once startup succeeds,
    /// the caller keeps both COM resources alive through the worker's join.
    pub fn run(self) -> Result<WorkerJoined<W::Output>, (std::io::Error, UnstartedDrain<W>)> {
        let completion = &self.completion;
        let terminated = OnceLock::new();
        std::thread::scope(|scope| {
            let publish_termination = &terminated;
            let transferred = self.drain.spawn(
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
            )?;
            completion
                .wait_with_com_pumping()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
            // The completion callback runs before the helper's TLS and OS
            // exit hooks. Keep dispatching through their synchronous COM calls.
            let helper = terminated
                .get()
                .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
            wait_with_com_pumping(helper.as_handle(), INFINITE)
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
            Ok(transferred.join())
        })
    }
}

fn duplicate_current_thread() -> OwnedHandle {
    let mut raw = ptr::null_mut();
    // SAFETY: these pseudo-handle getters have no caller preconditions.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: this returns the helper's current-thread pseudo-handle.
    let thread = unsafe { GetCurrentThread() };
    // SAFETY: both process pseudo-handles designate this process; the thread
    // pseudo-handle designates this helper. The output points to live storage.
    let success =
        unsafe { DuplicateHandle(process, thread, process, &mut raw, SYNCHRONIZE as u32, 0, 0) };
    if success == 0 || raw.is_null() {
        xlfn_kernel::invariant::fail_stop();
    }
    // SAFETY: successful duplication issued a real handle uniquely owned here.
    unsafe { OwnedHandle::from_raw_handle(raw) }
}
