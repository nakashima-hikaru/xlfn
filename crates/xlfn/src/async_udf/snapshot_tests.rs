use super::{AsyncExecutor, AsyncRuntime, AsyncTask, CalculationEpoch};
use crate::sync::Mutex;
use crate::{AsyncTaskLimit, ExcelError, XllError, XllResult};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct ExecutorState {
    tasks: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>,
    reserve_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    starts: AtomicUsize,
    stops: AtomicUsize,
    reserves: AtomicUsize,
    submitted: AtomicUsize,
    permit_drops: AtomicUsize,
    executor_drops: AtomicUsize,
}

struct HoldingExecutor(Arc<ExecutorState>);

struct ExecutorPermit(Arc<ExecutorState>);

impl Drop for ExecutorPermit {
    fn drop(&mut self) {
        self.0.permit_drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for HoldingExecutor {
    fn drop(&mut self) {
        self.0.executor_drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl AsyncExecutor for HoldingExecutor {
    type Reservation = ExecutorPermit;

    fn start(&self) -> XllResult<()> {
        self.0.starts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn reserve(&self) -> XllResult<Self::Reservation> {
        self.0.reserves.fetch_add(1, Ordering::Relaxed);
        let hook = self.0.reserve_hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
        Ok(ExecutorPermit(Arc::clone(&self.0)))
    }

    fn submit<F>(&self, _permit: Self::Reservation, task: AsyncTask<F>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.0.submitted.fetch_add(1, Ordering::Relaxed);
        self.0.tasks.lock().push(Box::pin(task));
    }

    fn shutdown(&self) -> XllResult<()> {
        assert!(self.0.tasks.lock().is_empty());
        self.0.stops.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn source() -> crate::cancellation::CancellationSource {
    crate::cancellation::CancellationSource::new(
        crate::cancellation::CancellationGuarantee::BestEffort,
    )
    .0
}

fn start_runtime(limit: AsyncTaskLimit) -> (AsyncRuntime<HoldingExecutor>, Arc<ExecutorState>) {
    let state = Arc::new(ExecutorState::default());
    let runtime = AsyncRuntime::new();
    runtime
        .start(HoldingExecutor(Arc::clone(&state)), limit)
        .unwrap();
    (runtime, state)
}

fn assert_cancelled<T>(result: XllResult<T>) {
    assert!(matches!(
        result,
        Err(XllError::ExcelValue(ExcelError::NotAvailable))
    ));
}

fn assert_unregistered(runtime: &AsyncRuntime<HoldingExecutor>) {
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 0);
    assert_eq!(runtime.registry().next_id.load(Ordering::Relaxed), 1);
}

#[test]
fn stale_snapshot_retains_calculation_until_rejected_reservation_releases_it() {
    let (runtime, state) = start_runtime(AsyncTaskLimit::DEFAULT);
    let snapshot = runtime.snapshot_calculation();
    let old = snapshot.epoch();
    assert_eq!(old, CalculationEpoch::INITIAL);
    assert_unregistered(&runtime);

    assert!(runtime.advance_calculation());
    {
        let control = runtime.registry().control.lock();
        assert_eq!(control.calculations[&old].pins.load(Ordering::Acquire), 1);
    }
    assert_eq!(snapshot.epoch(), old);
    assert_cancelled(runtime.reserve_snapshot(snapshot));
    assert_unregistered(&runtime);
    assert_eq!(state.reserves.load(Ordering::Relaxed), 1);
    assert_eq!(state.permit_drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        runtime.registry().control.lock().calculations[&old]
            .pins
            .load(Ordering::Acquire),
        0
    );

    assert!(runtime.advance_calculation());
    assert!(
        !runtime
            .registry()
            .control
            .lock()
            .calculations
            .contains_key(&old)
    );
    assert!(runtime.close().is_ok());
}

#[test]
fn cancelled_snapshot_cannot_register_a_task() {
    let (runtime, state) = start_runtime(AsyncTaskLimit::DEFAULT);
    let snapshot = runtime.snapshot_calculation();
    runtime.cancel_current_calculation();
    assert_cancelled(runtime.reserve_snapshot(snapshot));
    assert_unregistered(&runtime);
    assert_eq!(state.submitted.load(Ordering::Relaxed), 0);
    assert_eq!(state.permit_drops.load(Ordering::Relaxed), 1);
    assert!(runtime.close().is_ok());
}

#[test]
fn snapshot_does_not_block_close_but_prevents_same_epoch_owner_replacement() {
    let (runtime, state) = start_runtime(AsyncTaskLimit::DEFAULT);
    let mut snapshot = Some(runtime.snapshot_calculation());
    assert_unregistered(&runtime);
    std::thread::scope(|scope| {
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let runtime = &runtime;
        let closer = scope.spawn(move || {
            let result = runtime.close();
            closed_tx.send(result.is_ok()).unwrap();
            result
        });
        let closed_with_snapshot = closed_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap_or(false);
        if !closed_with_snapshot {
            // Release the witness before reporting a regression so a close
            // mistakenly waiting for snapshots can finish the scoped thread.
            drop(snapshot.take());
        }
        assert!(closer.join().unwrap().is_ok());
        assert!(closed_with_snapshot, "conversion snapshot blocked shutdown");
    });
    assert!(runtime.is_stopped());
    assert_eq!(state.stops.load(Ordering::Relaxed), 1);
    assert_eq!(state.executor_drops.load(Ordering::Relaxed), 1);

    assert!(matches!(
        runtime.start(HoldingExecutor(Arc::clone(&state)), AsyncTaskLimit::DEFAULT),
        Err(XllError::Closing)
    ));
    assert!(runtime.is_stopped());
    assert_eq!(
        snapshot.as_ref().unwrap().epoch(),
        CalculationEpoch::INITIAL
    );
    assert_eq!(state.stops.load(Ordering::Relaxed), 2);
    assert_eq!(state.executor_drops.load(Ordering::Relaxed), 2);

    drop(snapshot.take());
    runtime
        .start(HoldingExecutor(Arc::clone(&state)), AsyncTaskLimit::DEFAULT)
        .unwrap();
    let fresh = runtime.snapshot_calculation();
    let reservation = runtime.reserve_snapshot(fresh).unwrap();
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 1);
    drop(reservation);
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 0);
    assert!(runtime.close().is_ok());
    assert_eq!(state.stops.load(Ordering::Relaxed), 3);
    assert_eq!(state.executor_drops.load(Ordering::Relaxed), 3);
}

#[test]
fn snapshot_from_another_registry_never_registers_a_task() {
    let (first, _) = start_runtime(AsyncTaskLimit::DEFAULT);
    let (second, second_state) = start_runtime(AsyncTaskLimit::DEFAULT);
    let foreign = first.snapshot_calculation();
    assert!(matches!(
        second.preflight_snapshot(&foreign),
        Err(XllError::Closing)
    ));
    assert!(matches!(
        second.reserve_snapshot(foreign),
        Err(XllError::Closing)
    ));
    assert_unregistered(&first);
    assert_unregistered(&second);
    assert_eq!(second_state.submitted.load(Ordering::Relaxed), 0);
    assert_eq!(
        second_state.permit_drops.load(Ordering::Relaxed),
        second_state.reserves.load(Ordering::Relaxed)
    );
    assert!(first.close().is_ok());
    assert!(second.close().is_ok());
}

#[test]
fn executor_reservation_reentry_invalidates_snapshot_before_registration() {
    for advance in [false, true] {
        let state = Arc::new(ExecutorState::default());
        let runtime = Arc::new(AsyncRuntime::new());
        runtime
            .start(HoldingExecutor(Arc::clone(&state)), AsyncTaskLimit::DEFAULT)
            .unwrap();
        let weak = Arc::downgrade(&runtime);
        *state.reserve_hook.lock() = Some(Arc::new(move || {
            let runtime = weak.upgrade().unwrap();
            if advance {
                assert!(runtime.advance_calculation());
            } else {
                runtime.cancel_current_calculation();
            }
        }));
        let snapshot = runtime.snapshot_calculation();
        assert_cancelled(runtime.reserve_snapshot(snapshot));
        assert_unregistered(&runtime);
        assert_eq!(state.reserves.load(Ordering::Relaxed), 1);
        assert_eq!(state.permit_drops.load(Ordering::Relaxed), 1);
        assert_eq!(state.submitted.load(Ordering::Relaxed), 0);
        assert!(runtime.close().is_ok());
    }
}

#[test]
fn snapshot_saturation_preserves_cancelled_and_closing_error_priority() {
    struct CountDrop(Arc<AtomicUsize>);
    impl Drop for CountDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    let (runtime, state) = start_runtime(AsyncTaskLimit::new(1).unwrap());
    let future_drops = Arc::new(AtomicUsize::new(0));
    let retained = CountDrop(Arc::clone(&future_drops));
    runtime
        .reserve_snapshot(runtime.snapshot_calculation())
        .unwrap()
        .commit(
            async move {
                let _retained = retained;
                std::future::pending::<()>().await;
            },
            source(),
        );
    assert_eq!(state.submitted.load(Ordering::Relaxed), 1);
    assert_eq!(future_drops.load(Ordering::Relaxed), 0);

    let saturated = runtime.snapshot_calculation();
    assert!(matches!(
        runtime.preflight_snapshot(&saturated),
        Err(XllError::Overloaded)
    ));
    assert!(matches!(
        runtime.reserve_snapshot(saturated),
        Err(XllError::Overloaded)
    ));
    assert_eq!(state.reserves.load(Ordering::Relaxed), 2);
    assert_eq!(state.permit_drops.load(Ordering::Relaxed), 2);

    let old = runtime.snapshot_calculation();
    assert!(runtime.advance_calculation());
    assert_cancelled(runtime.preflight_snapshot(&old));
    assert_cancelled(runtime.reserve_snapshot(old));
    assert_eq!(state.reserves.load(Ordering::Relaxed), 3);
    assert_eq!(state.permit_drops.load(Ordering::Relaxed), 3);

    let closing = runtime.snapshot_calculation();
    runtime.registry().close_admission();
    assert!(matches!(
        runtime.preflight_snapshot(&closing),
        Err(XllError::Closing)
    ));
    assert!(matches!(
        runtime.reserve_snapshot(closing),
        Err(XllError::Closing)
    ));
    assert_eq!(runtime.registry().next_id.load(Ordering::Relaxed), 2);
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 1);
    assert_eq!(
        state.permit_drops.load(Ordering::Relaxed),
        state.reserves.load(Ordering::Relaxed)
    );
    let tasks = std::mem::take(&mut *state.tasks.lock());
    drop(tasks);
    assert_eq!(future_drops.load(Ordering::Relaxed), 1);
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 0);
    assert!(runtime.close().is_ok());
}

#[test]
fn miri_snapshot_final_pin_release_can_race_calculation_retirement() {
    let (runtime, _) = start_runtime(AsyncTaskLimit::DEFAULT);
    for _ in 0..8 {
        let snapshot = runtime.snapshot_calculation();
        let retired_epoch = snapshot.epoch();
        assert!(runtime.advance_calculation());
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let barrier = &barrier;
            let release = scope.spawn(move || {
                barrier.wait();
                assert_eq!(snapshot.epoch(), retired_epoch);
                drop(snapshot);
            });
            barrier.wait();
            // Retirement can observe either the retained pin or its final
            // release. In either ordering it must not invalidate the holder's
            // final read or decrement; Miri checks those production accesses.
            assert!(runtime.advance_calculation());
            release.join().unwrap();
        });
        assert!(runtime.advance_calculation());
        assert!(
            !runtime
                .registry()
                .control
                .lock()
                .calculations
                .contains_key(&retired_epoch)
        );
    }
    assert_unregistered(&runtime);
    assert!(runtime.close().is_ok());
}
