use super::{AsyncExecutor, AsyncRuntime, AsyncTask, CalculationEpoch};
use crate::sync::Mutex;
use crate::{AsyncTaskLimit, XllError, XllResult};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Default)]
struct State {
    tasks: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>,
    reject: AtomicBool,
    stop_failure: AtomicBool,
    stops: AtomicUsize,
    starts: AtomicUsize,
}

struct HoldingExecutor(Arc<State>);
impl AsyncExecutor for HoldingExecutor {
    type Reservation = ();
    fn start(&self) -> XllResult<()> {
        self.0.starts.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn reserve(&self) -> XllResult<()> {
        if self.0.reject.load(Ordering::Relaxed) {
            Err(XllError::Overloaded)
        } else {
            Ok(())
        }
    }
    fn submit<F>(&self, (): (), task: AsyncTask<F>)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.0.tasks.lock().push(Box::pin(task));
    }
    fn shutdown(&self) -> XllResult<()> {
        assert!(
            self.0.tasks.lock().is_empty(),
            "registry must drain before executor stop"
        );
        self.0.stops.fetch_add(1, Ordering::Relaxed);
        if self.0.stop_failure.load(Ordering::Relaxed) {
            Err(XllError::Panic)
        } else {
            Ok(())
        }
    }
}

fn source() -> crate::cancellation::CancellationSource {
    crate::cancellation::CancellationSource::new(
        crate::cancellation::CancellationGuarantee::BestEffort,
    )
    .0
}

#[test]
fn external_reservation_rejection_precedes_framework_task_registration() {
    let state = Arc::new(State::default());
    state.reject.store(true, Ordering::Relaxed);
    let runtime = AsyncRuntime::new();
    runtime
        .start(HoldingExecutor(Arc::clone(&state)), AsyncTaskLimit::DEFAULT)
        .unwrap();
    assert!(matches!(
        runtime.reserve(CalculationEpoch::INITIAL),
        Err(XllError::Overloaded)
    ));
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 0);
    assert_eq!(runtime.registry().next_id.load(Ordering::Relaxed), 1);
    assert!(runtime.close().unwrap().issues.is_empty());
}

#[test]
fn miri_external_unpolled_task_keeps_registry_alive_until_final_destruction() {
    struct CountDrop(Arc<AtomicUsize>);
    impl Drop for CountDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }
    let state = Arc::new(State::default());
    let runtime = Arc::new(AsyncRuntime::new());
    runtime
        .start(HoldingExecutor(Arc::clone(&state)), AsyncTaskLimit::DEFAULT)
        .unwrap();
    let dropped = Arc::new(AtomicUsize::new(0));
    let retained = CountDrop(Arc::clone(&dropped));
    runtime
        .submit(
            CalculationEpoch::INITIAL,
            async move {
                let _retained = retained;
                std::future::pending::<()>().await;
            },
            source(),
        )
        .unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let closing = Arc::clone(&runtime);
    let closer = std::thread::spawn(move || {
        done_tx.send(closing.close().is_ok()).unwrap();
    });
    assert!(runtime.wait_for_closing(Duration::from_secs(1)));
    assert!(done_rx.recv_timeout(Duration::from_millis(10)).is_err());
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 1);
    assert_eq!(state.stops.load(Ordering::Relaxed), 0);
    let tasks = std::mem::take(&mut *state.tasks.lock());
    drop(tasks);
    assert!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap());
    closer.join().unwrap();
    assert_eq!(dropped.load(Ordering::Acquire), 1);
    assert_eq!(state.stops.load(Ordering::Relaxed), 1);
}

#[test]
fn external_shutdown_failure_withholds_stop_certificate_and_can_retry() {
    let state = Arc::new(State::default());
    state.stop_failure.store(true, Ordering::Relaxed);
    let runtime = AsyncRuntime::new();
    runtime
        .start(HoldingExecutor(Arc::clone(&state)), AsyncTaskLimit::DEFAULT)
        .unwrap();
    assert!(matches!(runtime.close(), Err(XllError::Panic)));
    assert!(!runtime.is_stopped());
    assert!(matches!(
        runtime.reserve(runtime.current_epoch()),
        Err(XllError::Closing)
    ));
    assert!(!runtime.advance_calculation());
    state.stop_failure.store(false, Ordering::Relaxed);
    assert!(runtime.close().is_ok());
    assert!(runtime.is_stopped());
    assert_eq!(state.stops.load(Ordering::Relaxed), 2);
}

#[test]
fn executor_start_can_reenter_calculation_control_without_framework_lock() {
    struct ReentrantExecutor(Arc<dyn Fn() + Send + Sync>);
    impl AsyncExecutor for ReentrantExecutor {
        type Reservation = ();
        fn start(&self) -> XllResult<()> {
            (self.0)();
            Ok(())
        }
        fn reserve(&self) -> XllResult<()> {
            Ok(())
        }
        fn submit<F>(&self, (): (), task: AsyncTask<F>)
        where
            F: Future<Output = ()> + Send + 'static,
        {
            drop(task);
        }
        fn shutdown(&self) -> XllResult<()> {
            Ok(())
        }
    }
    let runtime = Arc::new(AsyncRuntime::<ReentrantExecutor>::new());
    let weak = Arc::downgrade(&runtime);
    let callback = Arc::new(move || {
        let runtime = weak.upgrade().unwrap();
        runtime.cancel_current_calculation();
        assert!(runtime.advance_calculation());
        assert_eq!(runtime.current_epoch().get(), 2);
        assert!(!runtime.is_running());
    });
    runtime
        .start(ReentrantExecutor(callback), AsyncTaskLimit::DEFAULT)
        .unwrap();
    assert_eq!(runtime.current_epoch().get(), 2);
    assert!(runtime.close().is_ok());
}

#[test]
fn successful_executor_stop_contains_panicking_disposal_and_finishes_state() {
    struct PanickingDisposal;
    impl Drop for PanickingDisposal {
        fn drop(&mut self) {
            panic!("executor destructor");
        }
    }
    impl AsyncExecutor for PanickingDisposal {
        type Reservation = ();
        fn start(&self) -> XllResult<()> {
            Ok(())
        }
        fn reserve(&self) -> XllResult<()> {
            Ok(())
        }
        fn submit<F>(&self, (): (), task: AsyncTask<F>)
        where
            F: Future<Output = ()> + Send + 'static,
        {
            drop(task);
        }
        fn shutdown(&self) -> XllResult<()> {
            Ok(())
        }
    }
    let runtime = AsyncRuntime::new();
    runtime
        .start(PanickingDisposal, AsyncTaskLimit::DEFAULT)
        .unwrap();
    let outcome = runtime.close().unwrap();
    assert_eq!(outcome.issues.len(), 1);
    assert_eq!(
        outcome.issues[0].kind,
        crate::shutdown::CleanupIssueKind::DisposalPanicked
    );
    assert!(runtime.is_stopped());
    assert!(runtime.close().is_ok());
}

#[test]
fn external_executor_polls_native_async_task_and_delivers_exactly_once() {
    struct ImmediateExecutor(Arc<AtomicUsize>);
    impl AsyncExecutor for ImmediateExecutor {
        type Reservation = ();
        fn start(&self) -> XllResult<()> {
            Ok(())
        }
        fn reserve(&self) -> XllResult<()> {
            Ok(())
        }
        fn submit<F>(&self, (): (), task: AsyncTask<F>)
        where
            F: Future<Output = ()> + Send + 'static,
        {
            let mut task = Box::pin(task);
            let mut cx = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
            self.0.fetch_add(1, Ordering::Relaxed);
            assert!(task.as_mut().poll(&mut cx).is_ready());
            drop(task);
        }
        fn shutdown(&self) -> XllResult<()> {
            Ok(())
        }
    }
    struct NativeAddin;
    impl crate::Addin for NativeAddin {
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();
        type AsyncExecutor = ImmediateExecutor;
        fn open(_: &crate::OpenContext) -> crate::OpenResult<Self> {
            unreachable!()
        }
    }
    let _test = crate::runtime::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _callback = crate::test_callback::lock();
    crate::test_callback::install();
    crate::test_callback::reset();
    let pointer = Box::into_raw(Box::new(crate::runtime::Runtime::<NativeAddin>::new()));
    struct Cleanup(*mut crate::runtime::Runtime<NativeAddin>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            // SAFETY: this test retains unique allocation ownership. The
            // synchronous executor destroys every task before returning;
            // Runtime's test destructor closes/drains services and exports.
            // No borrowed task, export reader, or runtime reference is used
            // after this final scope guard reclaims the allocation.
            unsafe {
                drop(Box::from_raw(self.0));
            }
        }
    }
    let _cleanup = Cleanup(pointer);
    // SAFETY: the owning scope guard keeps the allocation live until every
    // native boundary operation and task has completed, and then drains it.
    let runtime = unsafe { &*pointer };
    let open = runtime.begin_open().unwrap();
    let mut open = runtime.publish(open, (), ());
    runtime.finish_open(&mut open, Vec::new()).unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    runtime
        .start_async(
            ImmediateExecutor(Arc::clone(&polls)),
            AsyncTaskLimit::DEFAULT,
        )
        .unwrap();
    let mut handle = xlfn_sys::XLOPER12 {
        value: xlfn_sys::XLOPER12Value {
            big_data: xlfn_sys::XLOPER12BigData {
                handle: xlfn_sys::XLOPER12BigDataHandle {
                    data: std::ptr::null_mut(),
                },
                byte_count: 0,
            },
        },
        xltype: xlfn_sys::XLTYPE_BIG_DATA,
    };
    // SAFETY: handle is an aligned live stack-local Excel async token, copied
    // during the boundary's synchronous admission portion.
    unsafe {
        super::boundary::async_udf_boundary_named(
            runtime,
            "external_native",
            "EXTERNAL.NATIVE",
            &mut handle,
            |_, _, _| Ok(async { Ok::<_, XllError>(42.0) }),
        );
    }
    assert_eq!(polls.load(Ordering::Relaxed), 1);
    assert_eq!(crate::test_callback::async_return_calls(), 1);
    assert_eq!(crate::test_callback::last_async_value(), 42);
    assert_eq!(
        runtime
            .async_runtime()
            .registry()
            .active_tasks
            .load(Ordering::Acquire),
        0
    );
    assert!(runtime.close_async().is_ok());
    assert_eq!(crate::test_callback::async_return_calls(), 1);
}

#[test]
fn registry_task_limit_is_atomic_across_external_executor_reservations() {
    let state = Arc::new(State::default());
    let runtime = Arc::new(AsyncRuntime::new());
    runtime
        .start(
            HoldingExecutor(Arc::clone(&state)),
            AsyncTaskLimit::new(3).unwrap(),
        )
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let mut submitters = Vec::new();
    for _ in 0..8 {
        let runtime = Arc::clone(&runtime);
        let barrier = Arc::clone(&barrier);
        submitters.push(std::thread::spawn(move || {
            barrier.wait();
            runtime.submit(CalculationEpoch::INITIAL, std::future::pending(), source())
        }));
    }
    let accepted = submitters
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .filter(|result| {
            assert!(result.is_ok() || matches!(result, Err(XllError::Overloaded)));
            result.is_ok()
        })
        .count();
    assert_eq!(accepted, 3);
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 3);
    let tasks = std::mem::take(&mut *state.tasks.lock());
    drop(tasks);
    assert_eq!(runtime.registry().active_tasks.load(Ordering::Acquire), 0);
    assert!(runtime.close().is_ok());
}
