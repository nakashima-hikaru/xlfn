use crate::XllError;
use crate::drain_wait::{DrainWork, UnstartedDrain, WorkerJoined};
#[cfg(test)]
use crate::win32::E_UNEXPECTED;
use crate::win32::{DuplicateHandle, GetCurrentProcess, GetCurrentThread, INFINITE, SYNCHRONIZE};
use std::os::windows::io::{AsHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::OnceLock;
#[cfg(any(feature = "rtd", feature = "handles"))]
pub(crate) use xlfn_host::com_wait::Win32EventError;
use xlfn_host::com_wait::wait_with_com_pumping;
pub(crate) use xlfn_host::com_wait::{ComApartmentGuard, ManualResetEvent};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drain_wait::fail_next_drain_spawn;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static_assertions::assert_not_impl_any!(DispatchPrepared<fn()>: Send, Sync);

    #[test]
    fn com_wait_initializes_an_uninitialized_framework_thread() {
        std::thread::spawn(|| {
            let completed = ManualResetEvent::new(true).unwrap();
            completed.wait_with_com_pumping_for(1_000).unwrap();
        })
        .join()
        .unwrap();
    }

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
