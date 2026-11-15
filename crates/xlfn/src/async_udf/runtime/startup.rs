//! Startup owns the executor and recovery responsibility until publication.
use super::*;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

pub(super) struct StartupClaim<'a, E: AsyncExecutor> {
    owner: RollbackGuard<StartupExecutor<'a, E>>,
}
struct ExecutorStarted<'a, E: AsyncExecutor> {
    owner: RollbackGuard<StartingExecutor<'a, E>>,
}
struct RegistryArmed<'a, E: AsyncExecutor> {
    // Unlock registry management and discard its unpublished candidate
    // before executor rollback can run callbacks.
    registry: crate::async_udf::registry::control::RegistryPreparedStart<'a>,
    owner: RollbackGuard<StartingExecutor<'a, E>>,
}

struct StartupExecutor<'a, E: AsyncExecutor> {
    runtime: &'a AsyncRuntime<E>,
    executor: PublishedOwner<E>,
}

// Acquired before start() so that partial starts also require shutdown.
struct StartingExecutor<'a, E: AsyncExecutor>(StartupExecutor<'a, E>);

impl<'a, E: AsyncExecutor> StartupClaim<'a, E> {
    pub(super) fn acquire(runtime: &'a AsyncRuntime<E>, executor: E) -> XllResult<Self> {
        {
            let mut state = runtime.state.lock();
            if !matches!(*state, ExecutorSlot::Stopped) {
                return Err(XllError::Closing);
            }
            *state = ExecutorSlot::Starting;
        }
        Ok(Self {
            owner: RollbackGuard::new(StartupExecutor {
                runtime,
                executor: PublishedOwner::new(executor),
            }),
        })
    }

    fn start(self) -> XllResult<ExecutorStarted<'a, E>> {
        let owner = RollbackGuard::new(StartingExecutor(self.owner.complete()));
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            owner.get().0.executor.start()
        }))
        .unwrap_or(Err(XllError::Panic))?;
        Ok(ExecutorStarted { owner })
    }

    pub(super) fn publish(self, limit: crate::addin_impl::AsyncTaskLimit) -> XllResult<()> {
        self.start()?.arm(limit)?.publish();
        Ok(())
    }
}

impl<'a, E: AsyncExecutor> ExecutorStarted<'a, E> {
    fn arm(self, limit: crate::addin_impl::AsyncTaskLimit) -> XllResult<RegistryArmed<'a, E>> {
        let registry = self.owner.get().0.runtime.registry().prepare_start(limit)?;
        Ok(RegistryArmed {
            registry,
            owner: self.owner,
        })
    }
}

impl<E: AsyncExecutor> RegistryArmed<'_, E> {
    fn publish(self) {
        let runtime = self.owner.get().0.runtime;
        let mut state = runtime.state.lock();
        // Only this startup owner can publish or roll back Starting; close
        // waits for it and cannot replace the canonical slot meanwhile.
        let StartingExecutor(StartupExecutor { executor, .. }) = self.owner.complete();
        let pointer = NonNull::from(executor.as_ref());
        *state = ExecutorSlot::Running(executor);
        runtime
            .publication
            .store(pointer.as_ptr(), Ordering::Release);
        self.registry.publish();
        drop(runtime.spawn_admission.control().seal().wait().reopen());
        runtime.state_changed.notify_all();
        drop(state);
        runtime
            .registry()
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::StartAsyncExecutor);
    }
}

impl<E: AsyncExecutor> StartupExecutor<'_, E> {
    fn restore_slot(self, stopped: bool) {
        let Self { runtime, executor } = self;
        // Executor callbacks and destruction run outside runtime locks.
        if stopped {
            let _ = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                drop(executor)
            }));
            *runtime.state.lock() = ExecutorSlot::Stopped;
        } else {
            *runtime.state.lock() = ExecutorSlot::Closing(Some(executor));
        }
        runtime.state_changed.notify_all();
    }
}

impl<E: AsyncExecutor> Rollback for StartupExecutor<'_, E> {
    fn rollback(self) {
        self.restore_slot(true);
    }
}

impl<E: AsyncExecutor> Rollback for StartingExecutor<'_, E> {
    fn rollback(self) {
        let stopped = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            self.0.executor.shutdown()
        }))
        .unwrap_or(Err(XllError::Panic))
        .is_ok();
        self.0.restore_slot(stopped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Weak};

    #[derive(Default)]
    struct Events {
        starts: AtomicUsize,
        stops: AtomicUsize,
        drops: AtomicUsize,
        stop_fails: std::sync::atomic::AtomicBool,
        start_panics: std::sync::atomic::AtomicBool,
        check_unlocked_on_stop: crate::sync::Mutex<Weak<AsyncRuntime<Executor>>>,
    }
    struct Executor(Arc<Events>);
    impl Drop for Executor {
        fn drop(&mut self) {
            self.0.drops.fetch_add(1, Ordering::Release);
        }
    }
    impl AsyncExecutor for Executor {
        type Reservation = ();
        fn start(&self) -> XllResult<()> {
            self.0.starts.fetch_add(1, Ordering::Relaxed);
            assert!(
                !self.0.start_panics.load(Ordering::Relaxed),
                "partial executor start panic"
            );
            Ok(())
        }
        fn reserve(&self) -> XllResult<()> {
            Ok(())
        }
        fn submit<F: Future<Output = ()> + Send + 'static>(
            &self,
            (): (),
            task: super::super::super::AsyncTask<F>,
        ) {
            drop(task);
        }
        fn shutdown(&self) -> XllResult<()> {
            self.0.stops.fetch_add(1, Ordering::Relaxed);
            if let Some(runtime) = self.0.check_unlocked_on_stop.lock().upgrade() {
                assert!(runtime.registry().control.try_lock().is_some());
            }
            if self.0.stop_fails.load(Ordering::Relaxed) {
                Err(XllError::Panic)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn miri_startup_abandonment_cleans_only_completed_stages() {
        for stage in 0..3 {
            let events = Arc::new(Events::default());
            let runtime = AsyncRuntime::new();
            let claim = StartupClaim::acquire(&runtime, Executor(Arc::clone(&events))).unwrap();
            assert!(runtime.publication.load(Ordering::Acquire).is_null());
            match stage {
                0 => drop(claim),
                1 => drop(claim.start().unwrap()),
                _ => drop(
                    claim
                        .start()
                        .unwrap()
                        .arm(crate::AsyncTaskLimit::DEFAULT)
                        .unwrap(),
                ),
            }
            assert!(runtime.is_stopped());
            assert!(runtime.publication.load(Ordering::Acquire).is_null());
            assert_eq!(
                events.starts.load(Ordering::Relaxed),
                usize::from(stage != 0)
            );
            assert_eq!(
                events.stops.load(Ordering::Relaxed),
                usize::from(stage != 0)
            );
            assert_eq!(events.drops.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    fn miri_startup_unwind_rolls_back_armed_registry() {
        let events = Arc::new(Events::default());
        let runtime = Arc::new(AsyncRuntime::new());
        *events.check_unlocked_on_stop.lock() = Arc::downgrade(&runtime);
        let error = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            let _armed = StartupClaim::acquire(&runtime, Executor(Arc::clone(&events)))
                .unwrap()
                .start()
                .unwrap()
                .arm(crate::AsyncTaskLimit::DEFAULT)
                .unwrap();
            panic!("after registry armed");
        }));
        assert!(error.is_err());
        assert!(runtime.is_stopped());
        assert!(matches!(
            runtime.registry().preflight_available(),
            Err(XllError::Closing)
        ));
        assert_eq!(events.drops.load(Ordering::Acquire), 1);
        // An aborted start leaves a genuinely restartable stopped registry.
        runtime
            .start(
                Executor(Arc::clone(&events)),
                crate::AsyncTaskLimit::DEFAULT,
            )
            .unwrap();
        assert!(runtime.is_running());
        runtime.close().unwrap();
        assert_eq!(events.drops.load(Ordering::Acquire), 2);
    }

    #[test]
    fn miri_startup_stop_failure_retains_executor_for_close_retry() {
        let events = Arc::new(Events::default());
        events.stop_fails.store(true, Ordering::Relaxed);
        let runtime = AsyncRuntime::new();
        let started = StartupClaim::acquire(&runtime, Executor(Arc::clone(&events)))
            .unwrap()
            .start()
            .unwrap();
        drop(started);
        assert!(matches!(
            *runtime.state.lock(),
            ExecutorSlot::Closing(Some(_))
        ));
        assert_eq!(events.drops.load(Ordering::Acquire), 0);
        events.stop_fails.store(false, Ordering::Relaxed);
        runtime.close().unwrap();
        assert!(runtime.is_stopped());
        assert_eq!(events.stops.load(Ordering::Relaxed), 2);
        assert_eq!(events.drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn miri_partial_start_panic_keeps_recovery_responsibility() {
        for stop_fails in [false, true] {
            let events = Arc::new(Events::default());
            events.start_panics.store(true, Ordering::Relaxed);
            events.stop_fails.store(stop_fails, Ordering::Relaxed);
            let runtime = AsyncRuntime::new();
            assert!(matches!(
                runtime.start(
                    Executor(Arc::clone(&events)),
                    crate::AsyncTaskLimit::DEFAULT
                ),
                Err(XllError::Panic)
            ));
            assert!(runtime.publication.load(Ordering::Acquire).is_null());
            assert_eq!(events.stops.load(Ordering::Relaxed), 1);
            if stop_fails {
                assert_eq!(events.drops.load(Ordering::Acquire), 0);
                assert!(matches!(
                    *runtime.state.lock(),
                    ExecutorSlot::Closing(Some(_))
                ));
                events.stop_fails.store(false, Ordering::Relaxed);
                runtime.close().unwrap();
            }
            assert!(runtime.is_stopped());
            assert_eq!(events.drops.load(Ordering::Acquire), 1);
        }
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "exercise safely forgotten startup claims"
    )]
    #[cfg_attr(
        miri,
        ignore = "intentionally leaks startup claims and their runtime owners"
    )]
    fn forgotten_startup_never_publishes_or_destroys_executor() {
        for stage in 0..3 {
            let events = Arc::new(Events::default());
            let runtime = AsyncRuntime::new();
            let claim = StartupClaim::acquire(&runtime, Executor(Arc::clone(&events))).unwrap();
            match stage {
                0 => std::mem::forget(claim),
                1 => std::mem::forget(claim.start().unwrap()),
                _ => std::mem::forget(
                    claim
                        .start()
                        .unwrap()
                        .arm(crate::AsyncTaskLimit::DEFAULT)
                        .unwrap(),
                ),
            }
            assert!(runtime.publication.load(Ordering::Acquire).is_null());
            assert!(matches!(*runtime.state.lock(), ExecutorSlot::Starting));
            assert_eq!(events.drops.load(Ordering::Acquire), 0);
            std::mem::forget(runtime);
        }
    }
}
