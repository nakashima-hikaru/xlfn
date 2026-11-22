//! Excel-facing error mapping for the host adapter's native drain.
use crate::XllError;
use crate::drain_wait::{DrainWork, UnstartedDrain};
#[cfg(any(feature = "async", feature = "rtd", feature = "handles"))]
pub(crate) use xlfn_host::com_wait::ComApartmentGuard;
#[cfg(any(test, feature = "rtd", feature = "handles"))]
pub(crate) use xlfn_host::com_wait::ManualResetEvent;
#[cfg(any(feature = "rtd", feature = "handles"))]
pub(crate) use xlfn_host::com_wait::Win32EventError;
use xlfn_host::drain::WorkerJoined;
use xlfn_host::drain::windows::{self, DrainPreparationError};
#[cfg(test)]
pub(crate) use xlfn_host::drain::windows::{DrainPreparationStage, fail_next_drain_preparation};

pub(crate) struct DispatchPrepared<W>(windows::DispatchPrepared<W>);

pub(crate) fn drain_with_com_dispatch<W: DrainWork>(
    drain: UnstartedDrain<W>,
) -> Result<DispatchPrepared<W>, (XllError, UnstartedDrain<W>)> {
    windows::prepare_com_dispatch(drain)
        .map(DispatchPrepared)
        .map_err(|(error, owner)| {
            let error = match error {
                DrainPreparationError::Apartment(code) => XllError::Native {
                    code,
                    message: "failed to initialize COM drain apartment".into(),
                },
                DrainPreparationError::Event(error) => XllError::Native {
                    code: error.code as i32,
                    message: "failed to create COM drain completion event".into(),
                },
                DrainPreparationError::Injected(stage) => XllError::Native {
                    code: crate::win32::E_UNEXPECTED,
                    message: format!("injected COM drain preparation failure: {stage:?}"),
                },
            };
            (error, owner)
        })
}

impl<W: DrainWork> DispatchPrepared<W> {
    pub(crate) fn run(self) -> Result<WorkerJoined<W::Output>, (XllError, UnstartedDrain<W>)> {
        self.0.run().map_err(|(error, owner)| {
            (
                XllError::Native {
                    code: error.raw_os_error().unwrap_or(0),
                    message: format!("failed to start COM drain worker: {error}"),
                },
                owner,
            )
        })
    }
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
        }
    }

    #[test]
    fn refused_spawn_maps_error_and_returns_unstarted_owner() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = DropObserved(Arc::clone(&drops));
        let ready = drain_with_com_dispatch(UnstartedDrain::new(move || {
            drop(owner);
        }))
        .unwrap_or_else(|_| panic!("preparing the apartment and event must succeed"));

        fail_next_drain_spawn();
        let Err((error, unstarted)) = ready.run() else {
            panic!("injected spawn failure started a worker");
        };
        assert!(matches!(error, XllError::Native { .. }));
        assert_eq!(drops.load(Ordering::Acquire), 0);
        drop(unstarted);
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
