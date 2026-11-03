mod startup;
use super::calculation::CalculationEpoch;
use super::executor::AsyncExecutor;
use super::registry::{
    CalculationSnapshot, RegistryClose, TaskRegistry, TaskReservation, TasksCancelled,
    TasksDrained, cancel_tasks,
};
#[cfg(any(test, all(feature = "bench-internals", feature = "async-builtin")))]
use crate::cancellation::CancellationSource;
use crate::cancellation::TaskCancellationOwner;
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use std::future::Future;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};
use xlfn_kernel::drain_gate::{DrainGate, DrainPermit};
use xlfn_kernel::published_owner::PublishedOwner;

#[derive(Debug)]
pub(crate) struct AsyncStopped {
    _private: (),
}

impl AsyncStopped {
    fn new() -> Self {
        Self { _private: () }
    }
}

/// Orchestration retains executor ownership independently of registry tasks.
/// A1: active_tasks is the sole task-lifetime authority. A6: a stopped
/// certificate requires registry drain followed by successful executor stop.
pub(crate) struct AsyncRuntime<E: AsyncExecutor> {
    registry: OnceLock<PublishedOwner<TaskRegistry>>,
    state: Mutex<ExecutorSlot<E>>,
    publication: AtomicPtr<E>,
    spawn_admission: DrainGate,
    state_changed: Condvar,
    #[cfg(test)]
    after_executor_snapshot: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    before_submit: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

pub(super) enum ExecutorSlot<E> {
    Stopped,
    Starting,
    Running(PublishedOwner<E>),
    Closing(Option<PublishedOwner<E>>),
}

mod closing;
#[cfg(test)]
use closing::ExecutorClose;

pub(crate) struct ExecutorRead<'a, E> {
    pointer: NonNull<E>,
    _permit: DrainPermit<'a>,
}

impl<E> Deref for ExecutorRead<'_, E> {
    type Target = E;
    fn deref(&self) -> &E {
        // SAFETY: close drains publication readers before dropping its owner.
        unsafe { self.pointer.as_ref() }
    }
}

/// Couples the pinned calculation with the only runtime allowed to reserve it.
/// Both fields are private, and only `AsyncRuntime::snapshot_calculation` creates it.
pub(crate) struct RuntimeCalculationSnapshot<'a, E: AsyncExecutor> {
    runtime: &'a AsyncRuntime<E>,
    calculation: CalculationSnapshot<'a>,
}

impl<'a, E: AsyncExecutor> RuntimeCalculationSnapshot<'a, E> {
    pub(crate) fn epoch(&self) -> CalculationEpoch {
        self.calculation.epoch()
    }

    pub(crate) fn preflight(&self) -> XllResult<()> {
        let runtime = self.runtime;
        if runtime.publication.load(Ordering::Acquire).is_null() {
            return Err(XllError::Closing);
        }
        if runtime.registry().preflight_available()? {
            return Ok(());
        }
        // The executor reader preserves the existing saturation/close ordering.
        let _executor = runtime.executor().ok_or(XllError::Closing)?;
        self.calculation.preflight()
    }

    pub(crate) fn reserve(self) -> XllResult<RuntimeReservation<'a, E>> {
        let runtime = self.runtime;
        let executor = runtime.executor().ok_or(XllError::Closing)?;
        #[cfg(test)]
        {
            let hook = runtime.after_executor_snapshot.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let permit = executor.reserve()?;
        // A snapshot pins storage only; it does not pre-admit the calculation.
        // Final reservation must re-enter admission and check capacity.
        let task = self.calculation.reserve()?;
        Ok(RuntimeReservation {
            task,
            permit,
            executor,
            #[cfg(test)]
            runtime,
        })
    }
}

pub(crate) struct RuntimeReservation<'a, E: AsyncExecutor> {
    // Registry reservation drops before executor publication admission.
    task: TaskReservation<'a>,
    permit: E::Reservation,
    executor: ExecutorRead<'a, E>,
    #[cfg(test)]
    runtime: &'a AsyncRuntime<E>,
}

impl<E: AsyncExecutor> RuntimeReservation<'_, E> {
    pub(super) fn commit_owned<F>(self, future: F, cancellation: TaskCancellationOwner)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = self.task.commit_owned(future, cancellation);
        #[cfg(test)]
        {
            let hook = self.runtime.before_submit.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        self.executor.submit(self.permit, task);
    }

    #[cfg(any(test, all(feature = "bench-internals", feature = "async-builtin")))]
    pub(crate) fn commit<F>(self, future: F, cancellation: CancellationSource)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = self.task.commit(future, cancellation);
        #[cfg(test)]
        {
            let hook = self.runtime.before_submit.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        self.executor.submit(self.permit, task);
    }

    #[cfg(all(feature = "handles", test, feature = "async-builtin"))]
    pub(crate) fn commit_handle_scoped<B>(
        self,
        generation: crate::generation::RuntimeGeneration,
        build: B,
        cancellation: CancellationSource,
    ) where
        B: super::task_scope::HandleScopedTaskBuilder + Send + 'static,
    {
        // Build only when the executor first polls the committed task. All
        // admission guards have been released before user code or cleanup can
        // reenter cancellation/calculation rotation.
        self.commit(
            async move {
                let brand = crate::handle::GenerationLeaseBrand;
                let scope = super::task_scope::AsyncTaskScope::new(generation, &brand);
                let (future, delivery) = build.build_task(scope);
                // The inner future borrows this task's brand until delivery and
                // destruction finish. The enclosing task owns all captured data,
                // so it remains 'static without erasing the inner lifetime.
                B::deliver(delivery, future).await;
            },
            cancellation,
        );
    }
}

impl<E: AsyncExecutor> AsyncRuntime<E> {
    pub(crate) const fn new() -> Self {
        Self {
            registry: OnceLock::new(),
            state: Mutex::new(ExecutorSlot::Stopped),
            publication: AtomicPtr::new(std::ptr::null_mut()),
            spawn_admission: DrainGate::new_sealed(),
            state_changed: Condvar::new(),
            #[cfg(test)]
            after_executor_snapshot: Mutex::new(None),
            #[cfg(test)]
            before_submit: Mutex::new(None),
        }
    }

    pub(crate) fn registry(&self) -> &TaskRegistry {
        self.registry
            .get_or_init(|| PublishedOwner::new(TaskRegistry::new()))
    }

    pub(crate) fn start(&self, executor: E, limit: crate::addin::AsyncTaskLimit) -> XllResult<()> {
        startup::StartupClaim::acquire(self, executor)?.publish(limit)
    }

    pub(crate) fn current_epoch(&self) -> CalculationEpoch {
        self.registry().current_epoch()
    }

    pub(crate) fn snapshot_calculation(&self) -> RuntimeCalculationSnapshot<'_, E> {
        RuntimeCalculationSnapshot {
            runtime: self,
            calculation: self.registry().snapshot(),
        }
    }

    fn executor(&self) -> Option<ExecutorRead<'_, E>> {
        let permit = self.spawn_admission.try_enter().ok()?;
        let pointer = NonNull::new(self.publication.load(Ordering::Acquire))?;
        Some(ExecutorRead {
            pointer,
            _permit: permit,
        })
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn preflight_submit(&self, epoch: CalculationEpoch) -> XllResult<()> {
        // The ordinary preflight neither dereferences the executor nor owns a
        // task. Only final reserve needs its authoritative publication reader.
        if self.publication.load(Ordering::Acquire).is_null() {
            return Err(XllError::Closing);
        }
        if self.registry().preflight_available()? {
            return Ok(());
        }
        // Saturation takes a temporary registry reservation. Keep publication
        // admission through that path so close cannot seal registry activity
        // between calculation admission and its capacity check.
        let _executor = self.executor().ok_or(XllError::Closing)?;
        self.registry().preflight(epoch)
    }

    #[cfg(any(test, all(feature = "bench-internals", feature = "async-builtin")))]
    pub(crate) fn reserve(&self, epoch: CalculationEpoch) -> XllResult<RuntimeReservation<'_, E>> {
        let executor = self.executor().ok_or(XllError::Closing)?;
        #[cfg(test)]
        {
            let hook = self.after_executor_snapshot.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let permit = executor.reserve()?;
        let task = self.registry().reserve(epoch)?;
        Ok(RuntimeReservation {
            task,
            permit,
            executor,
            #[cfg(test)]
            runtime: self,
        })
    }

    #[cfg(any(test, all(feature = "bench-internals", feature = "async-builtin")))]
    pub(crate) fn submit<F>(
        &self,
        epoch: CalculationEpoch,
        future: F,
        cancellation: CancellationSource,
    ) -> XllResult<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        match self.reserve(epoch) {
            Ok(reservation) => {
                reservation.commit(future, cancellation);
                Ok(())
            }
            Err(error) => {
                if !matches!(error, XllError::Overloaded) {
                    super::registry::cancel_source_no_unwind(&cancellation);
                }
                drop(future);
                Err(error)
            }
        }
    }

    pub(crate) fn cancel_current_calculation(&self) {
        let tasks = self.registry().cancel_calculation(self.current_epoch());
        cancel_tasks(tasks);
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn cancel_calculation(&self, epoch: CalculationEpoch) {
        let tasks = self.registry().cancel_calculation(epoch);
        cancel_tasks(tasks);
    }

    pub(crate) fn advance_calculation(&self) -> bool {
        self.registry().advance()
    }

    pub(crate) fn close(&self) -> XllResult<crate::shutdown::StopOutcome<AsyncStopped>> {
        let Some(closing) = self.take_for_close() else {
            return Ok(crate::shutdown::StopOutcome {
                certificate: AsyncStopped::new(),
                issues: Vec::new(),
            });
        };
        closing.drain_readers()?.wait().shutdown()
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn close_with_timeout(&self, timeout: std::time::Duration) -> XllResult<()> {
        let Some(closing) = self.take_for_close() else {
            return Ok(());
        };
        closing
            .drain_readers()?
            .wait_timeout(timeout)?
            .shutdown()?
            .issues
            .into_iter()
            .next()
            .map_or(Ok(()), |issue| Err(issue.error))
    }

    #[cfg(any(
        all(test, feature = "async-builtin"),
        all(feature = "bench-internals", feature = "async-builtin")
    ))]
    pub(crate) fn wait_idle(&self) -> bool {
        self.registry().wait_idle();
        true
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        matches!(*self.state.lock(), ExecutorSlot::Running(_))
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(super) fn executor_state(&self) -> impl Deref<Target = ExecutorSlot<E>> + '_ {
        self.state.lock()
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(super) fn try_executor_state(&self) -> Option<impl Deref<Target = ExecutorSlot<E>> + '_> {
        self.state.try_lock()
    }
    pub(crate) fn is_stopped(&self) -> bool {
        matches!(*self.state.lock(), ExecutorSlot::Stopped)
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.registry().observer.set_trace_sink(trace);
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn set_after_executor_snapshot_hook(
        &self,
        hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.after_executor_snapshot.lock() = hook;
    }
    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn set_before_submit_hook(
        &self,
        hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.before_submit.lock() = hook;
    }
    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn set_after_calculation_publish_hook(
        &self,
        hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.registry().after_publish_hook.lock() = hook;
    }
    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn set_before_calculation_transition_hook(
        &self,
        hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.registry().before_transition_hook.lock() = hook;
    }
    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn set_after_calculation_snapshot_hook(
        &self,
        hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.registry().after_snapshot_hook.lock() = hook;
    }
    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn set_after_calculation_admission_hook(
        &self,
        hook: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.registry().after_admission_hook.lock() = hook;
    }
    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn snapshot_executor(&self) -> Option<ExecutorRead<'_, E>> {
        self.executor()
    }
    #[cfg(test)]
    pub(crate) fn wait_for_closing(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.state.lock();
        while !matches!(*state, ExecutorSlot::Closing(_)) {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            self.state_changed.wait_for(&mut state, deadline - now);
        }
        true
    }
}

impl<E: AsyncExecutor> Drop for AsyncRuntime<E> {
    fn drop(&mut self) {
        if self.close().is_err() {
            // Failed shutdown grants no certificate. Retain both affine
            // resources rather than reclaim memory still used by executor.
            let state = std::mem::replace(self.state.get_mut(), ExecutorSlot::Stopped);
            let _retained = std::mem::ManuallyDrop::new(state);
            if let Some(registry) = self.registry.take() {
                let _retained = std::mem::ManuallyDrop::new(registry);
            }
        }
    }
}

#[cfg(test)]
mod close_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[derive(Default)]
    struct Counters {
        stops: AtomicUsize,
        drops: AtomicUsize,
        panic_on_shutdown: AtomicBool,
    }

    struct Executor(Arc<Counters>);

    impl Drop for Executor {
        fn drop(&mut self) {
            self.0.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl AsyncExecutor for Executor {
        type Reservation = ();
        fn start(&self) -> XllResult<()> {
            Ok(())
        }
        fn reserve(&self) -> XllResult<()> {
            Ok(())
        }
        fn submit<F>(&self, (): (), task: super::super::AsyncTask<F>)
        where
            F: Future<Output = ()> + Send + 'static,
        {
            drop(task);
        }
        fn shutdown(&self) -> XllResult<()> {
            self.0.stops.fetch_add(1, Ordering::Relaxed);
            assert!(
                !self.0.panic_on_shutdown.load(Ordering::Relaxed),
                "injected shutdown panic"
            );
            Ok(())
        }
    }

    fn start() -> (AsyncRuntime<Executor>, Arc<Counters>) {
        let counters = Arc::new(Counters::default());
        let runtime = AsyncRuntime::new();
        runtime
            .start(
                Executor(Arc::clone(&counters)),
                crate::AsyncTaskLimit::DEFAULT,
            )
            .unwrap();
        (runtime, counters)
    }

    fn abandon<S>(closing: ExecutorClose<'_, Executor, S>, unwind: bool) {
        assert!(!unwind, "injected close abandonment");
        drop(closing);
    }

    fn assert_retained(runtime: &AsyncRuntime<Executor>, counters: &Counters) {
        assert!(matches!(
            *runtime.state.lock(),
            ExecutorSlot::Closing(Some(_))
        ));
        assert!(runtime.executor().is_none());
        assert!(!runtime.is_stopped());
        assert_eq!(counters.drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn abandoned_executor_close_stages_restore_owner_and_release_registry_claim() {
        for stage in 0..3 {
            for unwind in [false, true] {
                let (runtime, counters) = start();
                let result =
                    crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                        let closing = runtime.take_for_close().unwrap();
                        match stage {
                            0 => abandon(closing, unwind),
                            1 => abandon(closing.drain_readers().unwrap(), unwind),
                            _ => abandon(closing.drain_readers().unwrap().wait(), unwind),
                        }
                    }));
                assert_eq!(result.is_err(), unwind);
                assert_retained(&runtime, &counters);
                assert_eq!(counters.stops.load(Ordering::Relaxed), 0);
                assert!(runtime.close().unwrap().issues.is_empty());
                assert_eq!(counters.stops.load(Ordering::Relaxed), 1);
                assert_eq!(counters.drops.load(Ordering::Relaxed), 1);
            }
        }
    }

    #[test]
    fn registry_close_rejection_restores_executor_for_retry() {
        let (runtime, counters) = start();
        let competing = runtime.registry().request_close().unwrap();
        assert!(matches!(runtime.close(), Err(XllError::Closing)));
        assert_retained(&runtime, &counters);
        assert_eq!(counters.stops.load(Ordering::Relaxed), 0);
        drop(competing);
        assert!(runtime.close().is_ok());
        assert_eq!(counters.drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn panicking_executor_shutdown_retains_owner_without_stopped_certificate() {
        let (runtime, counters) = start();
        counters.panic_on_shutdown.store(true, Ordering::Relaxed);
        assert!(matches!(runtime.close(), Err(XllError::Panic)));
        assert_retained(&runtime, &counters);
        counters.panic_on_shutdown.store(false, Ordering::Relaxed);
        assert!(runtime.close().unwrap().issues.is_empty());
        assert!(runtime.is_stopped());
        assert_eq!(counters.stops.load(Ordering::Relaxed), 2);
        assert_eq!(counters.drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn abandoned_executor_close_wakes_a_waiting_closer() {
        let (runtime, counters) = start();
        let closing = runtime
            .take_for_close()
            .unwrap()
            .drain_readers()
            .unwrap()
            .wait();
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let runtime = &runtime;
            let waiting = scope.spawn(move || {
                started_tx.send(()).unwrap();
                done_tx.send(runtime.close().is_ok()).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(
                done_rx
                    .recv_timeout(std::time::Duration::from_millis(10))
                    .is_err()
            );
            drop(closing);
            assert!(
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap()
            );
            waiting.join().unwrap();
        });
        assert_eq!(counters.stops.load(Ordering::Relaxed), 1);
        assert_eq!(counters.drops.load(Ordering::Relaxed), 1);
    }
}
