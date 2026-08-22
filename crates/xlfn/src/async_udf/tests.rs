use super::*;

use crate::{Addin, OpenContext};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::runtime::tests::TEST_LOCK;
const TEST_GENERATION: u64 = 1;
static EVALUATION_BARRIER: Mutex<
    Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
> = Mutex::new(None);

struct AsyncTestGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    cleanup: Option<Box<dyn FnOnce()>>,
}

impl Drop for AsyncTestGuard {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            let ingress = crate::ingress::global_ingress();
            if ingress.phase() != crate::ingress::PHASE_CLOSED {
                ingress.begin_close_with(|| {});
                let _ = ingress.seal_and_drain();
            }
            cleanup();
        }
    }
}

fn test_lock() -> AsyncTestGuard {
    AsyncTestGuard {
        _lock: TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner()),
        cleanup: None,
    }
}

fn test_lock_for_runtime<A: Addin>(runtime: &'static Runtime<A>) -> AsyncTestGuard {
    AsyncTestGuard {
        _lock: TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner()),
        cleanup: Some(Box::new(move || runtime.release_test_module_lease())),
    }
}

struct TestU32Addin;

impl Addin for TestU32Addin {
    type State = u32;
    type Error = XllError;
    type Layers = ();

    fn open(
        _: &OpenContext,
    ) -> Result<crate::addin::Opened<Self::State, Self::Layers>, Self::Error> {
        unreachable!()
    }
}

fn stop_after_async_evaluation() {
    let barrier = EVALUATION_BARRIER.lock();
    let (reached, release) = barrier.as_ref().expect("evaluation barrier is installed");
    reached.send(()).unwrap();
    release.recv().unwrap();
}

fn test_cancellation_source() -> CancellationSource {
    CancellationSource::new(CancellationGuarantee::BestEffort).0
}

fn reset_test_callback() -> crate::test_callback::CallbackTestGuard {
    let guard = crate::test_callback::lock();
    crate::test_callback::install();
    crate::test_callback::reset();
    guard
}

fn wait_for_async_callback() -> i32 {
    let deadline = Instant::now() + Duration::from_secs(1);
    while crate::test_callback::async_return_calls() == 0 {
        assert!(Instant::now() < deadline, "async callback was not invoked");
        std::thread::yield_now();
    }
    crate::test_callback::last_async_value()
}

#[test]
fn executor_runs_tasks_and_joins_on_close() {
    let manager = AsyncManager::new();
    manager.start(2).unwrap();
    let completed = Arc::new(AtomicBool::new(false));
    let task_completed = Arc::clone(&completed);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                task_completed.store(true, Ordering::Release);
                done_tx.send(()).unwrap();
            },
            test_cancellation_source(),
        )
        .unwrap();
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(manager.close().issues.is_empty());
    assert!(completed.load(Ordering::Acquire));
}

#[test]
fn cancellation_drops_pending_future_without_running_its_tail() {
    struct DropSignal(Arc<AtomicBool>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    let manager = AsyncManager::new();
    manager.start(2).unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let signal = DropSignal(Arc::clone(&dropped));
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                let _signal = signal;
                std::future::pending::<()>().await;
            },
            test_cancellation_source(),
        )
        .unwrap();
    manager.cancel_generation(TEST_GENERATION);
    assert!(manager.close().issues.is_empty());
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn rejected_spawn_drops_future_after_releasing_manager_state() {
    struct ReentrantRejectedFuture {
        manager: Arc<AsyncManager>,
        dropped: std::sync::mpsc::Sender<()>,
    }

    impl Future for ReentrantRejectedFuture {
        type Output = ();

        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }

    impl Drop for ReentrantRejectedFuture {
        fn drop(&mut self) {
            self.manager.cancel_generation(TEST_GENERATION);
            self.dropped.send(()).unwrap();
        }
    }

    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    manager.cancel_generation(TEST_GENERATION);
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let future = ReentrantRejectedFuture {
        manager: Arc::clone(&manager),
        dropped: dropped_tx,
    };
    let spawning_manager = Arc::clone(&manager);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let spawning = std::thread::spawn(move || {
        result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, future, test_cancellation_source()))
            .unwrap();
    });

    assert!(matches!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    spawning.join().unwrap();
    assert!(manager.close().issues.is_empty());
}

#[test]
fn spawn_handle_snapshot_is_revalidated_after_generation_advance() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (snapshot_tx, snapshot_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    manager.set_after_spawn_handle_snapshot_hook(Some(Arc::new(move || {
        snapshot_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn snapshot should be released");
    })));

    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
            .unwrap();
    });

    snapshot_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("spawn should snapshot the executor handle");
    assert!(manager.advance_generation());
    release_tx.send(()).unwrap();

    assert!(matches!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    assert!(token.is_cancelled());
    spawning.join().unwrap();
    manager.set_after_spawn_handle_snapshot_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn concurrent_generation_advances_are_serialized() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let hook_barrier = Arc::clone(&barrier);
    manager.set_before_generation_transition_hook(Some(Arc::new(move || {
        hook_barrier.wait();
    })));

    let first_manager = Arc::clone(&manager);
    let first = std::thread::spawn(move || first_manager.advance_generation());
    let second_manager = Arc::clone(&manager);
    let second = std::thread::spawn(move || second_manager.advance_generation());

    assert!(first.join().unwrap());
    assert!(second.join().unwrap());
    assert_eq!(manager.current_generation(), TEST_GENERATION + 2);
    manager
        .spawn(TEST_GENERATION + 2, async {}, test_cancellation_source())
        .unwrap();

    manager.set_before_generation_transition_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn task_scheduling_does_not_hold_manager_state() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    manager.set_before_task_schedule_hook(Some(Arc::new(move || {
        admitted_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("task scheduling should be released");
    })));

    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        spawn_result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
            .unwrap();
    });
    admitted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("task should be admitted before scheduling");

    let cancelling_manager = Arc::clone(&manager);
    let (cancel_done_tx, cancel_done_rx) = std::sync::mpsc::sync_channel(1);
    let cancelling = std::thread::spawn(move || {
        cancelling_manager.cancel_generation(TEST_GENERATION);
        cancel_done_tx.send(()).unwrap();
    });
    cancel_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("cancellation should not wait for task scheduling");
    assert!(token.is_cancelled());

    release_tx.send(()).unwrap();
    assert!(
        spawn_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    spawning.join().unwrap();
    cancelling.join().unwrap();
    manager.set_before_task_schedule_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn close_rejects_a_spawn_using_a_snapshot_handle() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (snapshot_tx, snapshot_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    manager.set_after_spawn_handle_snapshot_hook(Some(Arc::new(move || {
        snapshot_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn snapshot should be released");
    })));

    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
            .unwrap();
    });
    snapshot_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("spawn should snapshot the executor handle");

    assert!(manager.close_with_timeout(Duration::from_secs(1)).is_ok());
    release_tx.send(()).unwrap();
    assert!(matches!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(XllError::Closing)
    ));
    assert!(token.is_cancelled());
    spawning.join().unwrap();
    manager.set_after_spawn_handle_snapshot_hook(None);
    assert!(manager.is_stopped());
}

#[test]
fn close_isolates_panicking_cancellation_waker_and_completes_shutdown() {
    struct PanicWake;

    impl std::task::Wake for PanicWake {
        fn wake(self: Arc<Self>) {
            panic!("injected async close waker panic");
        }

        fn wake_by_ref(self: &Arc<Self>) {
            panic!("injected async close waker panic");
        }
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let panic_waker = std::task::Waker::from(Arc::new(PanicWake));
    let mut waiter = Box::pin(token.cancelled());
    assert_eq!(
        waiter
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&panic_waker)),
        std::task::Poll::Pending
    );
    let dropped = Arc::new(AtomicBool::new(false));
    let drop_signal = DropSignal(Arc::clone(&dropped));
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                let _drop_signal = drop_signal;
                std::future::pending::<()>().await;
            },
            source,
        )
        .unwrap();

    assert!(manager.close().issues.is_empty());
    assert!(token.is_cancelled());
    assert!(dropped.load(Ordering::Acquire));

    // A completed close must leave no orphaned Closing(None) owner.
    assert!(manager.advance_generation());
    manager.start(1).unwrap();
    assert!(manager.close().issues.is_empty());
}

#[test]
fn close_allows_aborted_future_drop_to_reenter_runtime() {
    let _guard = test_lock();
    struct ReentrantDrop {
        runtime: &'static Runtime<()>,
        dropped: std::sync::mpsc::Sender<()>,
    }

    impl Drop for ReentrantDrop {
        fn drop(&mut self) {
            cancel_async_calculation(self.runtime);
            end_async_calculation(self.runtime);
            self.dropped.send(()).unwrap();
        }
    }

    let runtime: &'static Runtime<()> = Box::leak(Box::new(Runtime::new()));
    runtime.start_async(1).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let reentrant = ReentrantDrop {
        runtime,
        dropped: dropped_tx,
    };
    runtime
        .async_manager()
        .spawn(
            runtime.calculation_id().get(),
            async move {
                let _reentrant = reentrant;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                std::future::pending::<()>().await;
            },
            test_cancellation_source(),
        )
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let closer = std::thread::spawn(move || {
        closed_tx
            .send(
                runtime
                    .async_manager()
                    .close_with_timeout(Duration::from_secs(2)),
            )
            .unwrap();
    });
    assert!(
        runtime
            .async_manager()
            .wait_for_closing(Duration::from_secs(1))
    );
    assert!(matches!(runtime.start_async(1), Err(XllError::Closing)));
    release_tx.send(()).unwrap();

    dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    closer.join().unwrap();
}

#[test]
fn close_allows_aborted_layer_cleanup_to_reenter_runtime() {
    struct ReentrantTestAddin;
    impl Addin for ReentrantTestAddin {
        type State = u32;
        type Error = XllError;
        type Layers = (ReentrantLayer,);
        fn open(
            _: &OpenContext,
        ) -> Result<crate::addin::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }
    }

    struct ReentrantLayer {
        on_exit: std::sync::Arc<dyn Fn() + Send + Sync + 'static>,
    }
    struct ReentrantLayerGuard {
        on_exit: std::sync::Arc<dyn Fn() + Send + Sync + 'static>,
    }

    impl crate::execution::UdfLayer for ReentrantLayer {
        type Guard = ReentrantLayerGuard;
        fn enter(&self, _: &crate::execution::CallMetadata) -> XllResult<Self::Guard> {
            Ok(ReentrantLayerGuard {
                on_exit: std::sync::Arc::clone(&self.on_exit),
            })
        }
    }

    impl crate::execution::UdfLayerGuard for ReentrantLayerGuard {
        fn exit(self, _: &crate::execution::CallOutcome<'_>) {
            (self.on_exit)();
        }
    }

    let runtime: &'static Runtime<ReentrantTestAddin> = Box::leak(Box::new(Runtime::new()));
    let _guard = test_lock_for_runtime(runtime);
    let (exited_tx, exited_rx) = std::sync::mpsc::channel();
    let on_exit = std::sync::Arc::new(move || {
        cancel_async_calculation(runtime);
        end_async_calculation(runtime);
        exited_tx.send(()).unwrap();
    });
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(7_u32, (ReentrantLayer { on_exit },));
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(1).unwrap();
    let _callback_guard = reset_test_callback();

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
    unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async_reentrant_layer_close",
            "TEST.ASYNC.REENTRANT.LAYER.CLOSE",
            &mut handle,
            move |_, _| {
                Ok(async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    std::future::pending::<()>().await;
                    Ok::<_, XllError>(42.0)
                })
            },
        );
    }
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let closer = std::thread::spawn(move || {
        closed_tx
            .send(
                runtime
                    .async_manager()
                    .close_with_timeout(Duration::from_secs(2)),
            )
            .unwrap();
    });
    assert!(
        runtime
            .async_manager()
            .wait_for_closing(Duration::from_secs(1))
    );
    release_tx.send(()).unwrap();

    exited_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    closer.join().unwrap();
}

#[test]
fn cancellation_token_is_signaled_before_task_abort() {
    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let observed = token.clone();
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                let _token = token;
                std::future::pending::<()>().await;
            },
            source,
        )
        .unwrap();
    manager.cancel_generation(TEST_GENERATION);
    assert!(observed.is_cancelled());
    assert!(manager.close().issues.is_empty());
}

#[test]
fn cancelled_generation_rejects_late_spawn_and_next_generation_accepts_work() {
    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    manager.cancel_generation(TEST_GENERATION);

    assert!(matches!(
        manager.spawn(
            TEST_GENERATION,
            std::future::pending(),
            test_cancellation_source(),
        ),
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));

    let next = TEST_GENERATION + 1;
    assert!(manager.advance_generation());
    assert!(matches!(
        manager.spawn(
            TEST_GENERATION,
            std::future::pending(),
            test_cancellation_source(),
        ),
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    manager
        .spawn(next, async {}, test_cancellation_source())
        .unwrap();
    assert!(manager.close().issues.is_empty());
}

#[test]
fn cancelling_new_generation_does_not_cancel_live_work_from_previous_generation() {
    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    let (old_source, old_token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    manager
        .spawn(TEST_GENERATION, std::future::pending(), old_source)
        .unwrap();

    let next = TEST_GENERATION + 1;
    assert!(manager.advance_generation());
    let (new_source, new_token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    manager
        .spawn(next, std::future::pending(), new_source)
        .unwrap();

    manager.cancel_generation(next);
    assert!(new_token.is_cancelled());
    assert!(!old_token.is_cancelled());

    assert!(manager.close().issues.is_empty());
    assert!(old_token.is_cancelled());
}

#[test]
fn spawn_and_cancel_are_linearized_by_generation_admission() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);

    let spawning_manager = Arc::clone(&manager);
    let spawning_barrier = Arc::clone(&barrier);
    let spawning = std::thread::spawn(move || {
        spawning_barrier.wait();
        spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source)
    });

    let cancelling_manager = Arc::clone(&manager);
    let cancelling_barrier = Arc::clone(&barrier);
    let cancelling = std::thread::spawn(move || {
        cancelling_barrier.wait();
        cancelling_manager.cancel_generation(TEST_GENERATION);
    });

    barrier.wait();
    let spawn_result = spawning.join().unwrap();
    cancelling.join().unwrap();

    match spawn_result {
        Ok(()) => assert!(token.is_cancelled()),
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable)) => {
            assert!(token.is_cancelled());
        }
        Err(error) => panic!("unexpected spawn result: {error}"),
    }
    assert!(matches!(
        manager.spawn(
            TEST_GENERATION,
            std::future::pending(),
            test_cancellation_source(),
        ),
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    assert!(manager.close().issues.is_empty());
}

#[test]
fn joined_worker_panic_is_a_cleanup_issue_with_a_stop_certificate() {
    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                panic!("injected task panic");
            },
            test_cancellation_source(),
        )
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    release_tx.send(()).unwrap();
    let outcome = manager.close();
    assert_eq!(outcome.issues.len(), 1);
    assert_eq!(
        outcome.issues[0].kind,
        crate::shutdown::CleanupIssueKind::WorkerPanickedAfterJoin
    );
    let _stopped = outcome.certificate;
    assert!(manager.is_stopped());
}

#[test]
fn lone_worker_panic_drops_tasks_left_on_the_queue() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                panic!("injected worker-fatal panic");
            },
            test_cancellation_source(),
        )
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    manager
        .spawn(
            TEST_GENERATION,
            std::future::pending(),
            test_cancellation_source(),
        )
        .unwrap();

    let closing = Arc::clone(&manager);
    let closer = std::thread::spawn(move || closing.close());
    release_tx.send(()).unwrap();
    let outcome = closer.join().unwrap();

    assert_eq!(outcome.issues.len(), 1);
    assert_eq!(
        outcome.issues[0].kind,
        crate::shutdown::CleanupIssueKind::WorkerPanickedAfterJoin
    );
    assert!(manager.is_stopped());
}

#[test]
fn pending_task_limit_is_reserved_atomically() {
    let manager = AsyncManager::new();
    manager.start(2).unwrap();
    for _ in 0..MAX_PENDING {
        manager
            .spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            )
            .unwrap();
    }
    assert!(matches!(
        manager.spawn(
            TEST_GENERATION,
            std::future::pending(),
            test_cancellation_source(),
        ),
        Err(XllError::Overloaded)
    ));
    manager.cancel_generation(TEST_GENERATION);
    manager.close_with_timeout(Duration::from_secs(2)).unwrap();
}

#[test]
fn shutdown_timeout_refuses_close_until_blocking_poll_returns() {
    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            },
            test_cancellation_source(),
        )
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        manager
            .close_with_timeout(Duration::from_millis(10))
            .is_err()
    );
    release_tx.send(()).unwrap();
    manager.close_with_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn production_close_waits_until_blocking_poll_returns() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    manager
        .spawn(
            TEST_GENERATION,
            async move {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            },
            test_cancellation_source(),
        )
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    let closer_manager = Arc::clone(&manager);
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let closer = std::thread::spawn(move || {
        assert!(closer_manager.close().issues.is_empty());
        closed_tx.send(()).unwrap();
    });
    assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());
    release_tx.send(()).unwrap();
    closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    closer.join().unwrap();
}

#[test]
fn async_handle_payload_is_deep_copied() {
    let mut bytes = vec![1_u8, 2, 3, 4];
    let original = bytes.as_mut_ptr();
    let mut raw = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle { data: original },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    // SAFETY: raw is a live, well-formed test async handle.
    let mut owned = unsafe { OwnedAsyncHandle::from_raw("test_payload", &mut raw) }.unwrap();
    // SAFETY: the owned value remains XLTYPE_BIG_DATA with a positive size.
    let copied = unsafe { owned.raw.value.big_data.handle.data };
    assert_ne!(copied, original);
    bytes.fill(9);
    assert_eq!(
        // SAFETY: copied points to the owned four-byte payload.
        unsafe { std::slice::from_raw_parts(copied, 4) },
        &[1, 2, 3, 4]
    );
    owned.complete();
}

#[test]
fn async_boundary_returns_completed_value_through_callback() {
    let runtime = Box::leak(Box::new(Runtime::<TestU32Addin>::new()));
    let _guard = test_lock_for_runtime(runtime);
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(7_u32, ());
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(2).unwrap();

    let _callback_guard = reset_test_callback();
    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
    unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async",
            "TEST.ASYNC",
            &mut handle,
            |_, token| {
                assert_eq!(token.guarantee(), CancellationGuarantee::BestEffort);
                Ok(async { Ok::<_, XllError>(42.0) })
            },
        );
    }
    assert_eq!(wait_for_async_callback(), 42);
    assert_eq!(crate::test_callback::free_calls(), 0);
    assert!(runtime.close_async().issues.is_empty());
}

#[test]
fn async_boundary_reports_handler_failures_to_layers() {
    struct Recorder(std::sync::mpsc::Sender<(UdfResultKind, Option<i32>, usize)>);
    struct RecorderGuard {
        sender: std::sync::mpsc::Sender<(UdfResultKind, Option<i32>, usize)>,
        concurrent_calls: usize,
    }
    impl crate::execution::UdfLayer for Recorder {
        type Guard = RecorderGuard;
        fn enter(&self, metadata: &crate::execution::CallMetadata) -> XllResult<Self::Guard> {
            Ok(RecorderGuard {
                sender: self.0.clone(),
                concurrent_calls: metadata.concurrent_calls,
            })
        }
    }
    impl crate::execution::UdfLayerGuard for RecorderGuard {
        fn exit(self, outcome: &crate::execution::CallOutcome<'_>) {
            self.sender
                .send((outcome.result, outcome.vendor_code, self.concurrent_calls))
                .unwrap();
        }
    }

    struct HandlerFailAddin;
    impl Addin for HandlerFailAddin {
        type State = u32;
        type Error = XllError;
        type Layers = (Recorder,);
        fn open(
            _: &OpenContext,
        ) -> Result<crate::addin::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }
    }

    let runtime = Box::leak(Box::new(Runtime::<HandlerFailAddin>::new()));
    let _guard = test_lock_for_runtime(runtime);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(7_u32, (Recorder(event_sender),));
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(2).unwrap();

    let _callback_guard = reset_test_callback();
    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
    unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async_failure",
            "TEST.ASYNC.FAILURE",
            &mut handle,
            |_, _| {
                Ok(async {
                    Err::<f64, _>(XllError::Native {
                        code: 73,
                        message: "injected async failure".to_owned(),
                    })
                })
            },
        );
    }
    assert_eq!(wait_for_async_callback(), -1);
    let event = event_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(event.0, UdfResultKind::VendorError);
    assert_eq!(event.1, Some(73));
    assert_eq!(event.2, 1);

    assert!(runtime.close_async().issues.is_empty());
}

#[test]
fn async_boundary_records_delivery_rejection_as_failure() {
    struct Recorder(std::sync::mpsc::Sender<UdfResultKind>);
    struct RecorderGuard(std::sync::mpsc::Sender<UdfResultKind>);

    impl crate::execution::UdfLayer for Recorder {
        type Guard = RecorderGuard;
        fn enter(&self, _: &crate::execution::CallMetadata) -> XllResult<Self::Guard> {
            Ok(RecorderGuard(self.0.clone()))
        }
    }

    impl crate::execution::UdfLayerGuard for RecorderGuard {
        fn exit(self, outcome: &crate::execution::CallOutcome<'_>) {
            self.0.send(outcome.result).unwrap();
        }
    }

    struct DeliveryRejectionAddin;
    impl Addin for DeliveryRejectionAddin {
        type State = u32;
        type Error = XllError;
        type Layers = (Recorder,);
        fn open(
            _: &OpenContext,
        ) -> Result<crate::addin::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }
    }

    let runtime = Box::leak(Box::new(Runtime::<DeliveryRejectionAddin>::new()));
    let _guard = test_lock_for_runtime(runtime);
    let (event_sender, event_receiver) = std::sync::mpsc::channel();
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(7_u32, (Recorder(event_sender),));
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(1).unwrap();
    let _callback_guard = reset_test_callback();
    crate::test_callback::set_async_rejected(true);

    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
    unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async_delivery_failure",
            "TEST.ASYNC.DELIVERY.FAILURE",
            &mut handle,
            |_, _| Ok(async { Ok::<_, XllError>(42.0) }),
        );
    }

    assert_eq!(
        event_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        UdfResultKind::InternalError
    );
    assert_eq!(crate::test_callback::async_return_calls(), 1);
    assert!(runtime.close_async().issues.is_empty());
}

#[test]
fn async_boundary_returns_error_on_cancellation() {
    let runtime = Box::leak(Box::new(Runtime::<TestU32Addin>::new()));
    let _guard = test_lock_for_runtime(runtime);
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(7_u32, ());
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(2).unwrap();

    let _callback_guard = reset_test_callback();
    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
    unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async_cancel",
            "TEST.ASYNC.CANCEL",
            &mut handle,
            move |_, _| {
                let release_rx = release_rx;
                Ok(async move {
                    let _ = release_rx.recv();
                    Ok::<_, XllError>(123.0)
                })
            },
        );
    }
    // Cancel all running async tasks. OwnedAsyncHandle::drop should fire and return error to hook.
    cancel_async_calculation(runtime);
    drop(release_tx);
    assert_eq!(wait_for_async_callback(), -1);

    assert!(runtime.close_async().issues.is_empty());
}

#[test]
fn pending_async_cancellation_after_terminal_gate_never_calls_excel() {
    let runtime = Box::leak(Box::new(Runtime::<TestU32Addin>::new()));
    let _guard = test_lock_for_runtime(runtime);
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(7_u32, ());
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(1).unwrap();

    let _callback_guard = reset_test_callback();
    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
    unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async_terminal_gate",
            "TEST.ASYNC.TERMINAL.GATE",
            &mut handle,
            move |_, _| {
                Ok(async move {
                    started_tx.send(()).unwrap();
                    std::future::pending::<()>().await;
                    Ok::<_, XllError>(123.0)
                })
            },
        );
    }
    // Ensure cancellation observes a task that has actually started. If
    // the task were still queued, dropping it would not exercise the
    // OwnedAsyncHandle fallback that must be suppressed by the gate.
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("terminal-gate task did not start");

    let invocation = crate::callback_gate::CallbackInvocationToken::new();
    let callback_gate = crate::callback_gate::enter_callback(&invocation).unwrap();
    callback_gate.observe(crate::return_value::ExcelCallbackStatus::Abort);
    drop(callback_gate);
    let callbacks_before_cancel = crate::test_callback::async_return_calls();
    cancel_async_calculation(runtime);
    assert!(runtime.close_async().issues.is_empty());
    assert_eq!(
        crate::test_callback::async_return_calls(),
        callbacks_before_cancel,
        "terminal callback gate must suppress async cancellation fallback while token is active"
    );
    drop(invocation);

    let next_token = crate::callback_gate::CallbackInvocationToken::new();
    assert!(crate::callback_gate::enter_callback(&next_token).is_ok());
}

#[test]
fn cancellation_after_evaluation_does_not_leak_the_return_block() {
    let runtime = Box::leak(Box::new(Runtime::<TestU32Addin>::new()));
    let _guard = test_lock_for_runtime(runtime);
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(7_u32, ());
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(1).unwrap();

    let _callback_guard = reset_test_callback();
    // Record the process-global allocation count only after this runtime
    // owns the module test lease and callback state has been reset. A
    // concurrent return-value test may otherwise free its own block after
    // this test samples the baseline.
    let before = crate::return_value::live_return_blocks();
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    *EVALUATION_BARRIER.lock() = Some((reached_tx, release_rx));
    *AFTER_ASYNC_EVALUATION_HOOK.lock() = Some(stop_after_async_evaluation);

    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
    unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async_cancel_after_evaluation",
            "TEST.ASYNC.CANCEL.AFTER.EVALUATION",
            &mut handle,
            |_, _| Ok(async { Ok::<_, XllError>("allocated return payload".to_owned()) }),
        );
    }

    reached_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    cancel_async_calculation(runtime);
    release_tx.send(()).unwrap();
    assert_eq!(wait_for_async_callback(), -1);
    assert!(runtime.close_async().issues.is_empty());

    assert_eq!(crate::return_value::live_return_blocks(), before);
    *AFTER_ASYNC_EVALUATION_HOOK.lock() = None;
    *EVALUATION_BARRIER.lock() = None;
}

#[test]
fn advance_generation_does_not_block_on_task_schedule_hook() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    manager.set_before_task_schedule_hook(Some(Arc::new(move || {
        admitted_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("task scheduling should be released");
    })));

    let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        spawn_result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
            .unwrap();
    });
    admitted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("task should be admitted before scheduling");

    let advancing_manager = Arc::clone(&manager);
    let (advance_done_tx, advance_done_rx) = std::sync::mpsc::sync_channel(1);
    let advancing = std::thread::spawn(move || {
        advance_done_tx
            .send(advancing_manager.advance_generation())
            .unwrap();
    });
    assert!(
        advance_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("advance_generation should not block while task schedule hook is held")
    );

    release_tx.send(()).unwrap();
    spawn_result_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    spawning.join().unwrap();
    advancing.join().unwrap();
    manager.set_before_task_schedule_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn spawn_registered_before_close_is_drained_safely() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
    manager.set_before_task_schedule_hook(Some(Arc::new(move || {
        registered_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("schedule hook should be released");
    })));

    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        spawn_result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
            .unwrap();
    });
    registered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("task should be registered");

    let closing_manager = Arc::clone(&manager);
    let (close_result_tx, close_result_rx) = std::sync::mpsc::sync_channel(1);
    let closing = std::thread::spawn(move || {
        close_result_tx
            .send(closing_manager.close_with_timeout(Duration::from_secs(1)))
            .unwrap();
    });

    release_tx.send(()).unwrap();
    spawning.join().unwrap();
    let _ = spawn_result_rx.recv_timeout(Duration::from_secs(1));

    assert!(
        close_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    closing.join().unwrap();
    assert!(token.is_cancelled());
    manager.set_before_task_schedule_hook(None);
    assert!(manager.is_stopped());
}

#[test]
fn old_generation_retained_entry_rejected_on_spawn() {
    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    let (source1, _token1) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    manager
        .spawn(TEST_GENERATION, std::future::pending::<()>(), source1)
        .unwrap();
    assert!(manager.advance_generation());
    assert_eq!(manager.current_generation(), TEST_GENERATION + 1);

    let (source2, token2) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let res = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source2);
    assert!(matches!(
        res,
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    assert!(token2.is_cancelled());
    assert!(manager.close().issues.is_empty());
}

#[test]
fn rejection_priority_old_generation_over_max_pending() {
    let manager = AsyncManager::new();
    manager.start(1).unwrap();
    for _ in 0..MAX_PENDING {
        let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let _ = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source);
    }
    let (source_curr, token_curr) =
        CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let res_curr = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source_curr);
    assert!(matches!(res_curr, Err(XllError::Overloaded)));
    assert!(!token_curr.is_cancelled());

    assert!(manager.advance_generation());
    let gen2 = TEST_GENERATION + 1;

    for _ in 0..MAX_PENDING {
        let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let _ = manager.spawn(gen2, std::future::pending::<()>(), source);
    }

    let (source_old, token_old) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let res_old = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source_old);
    assert!(matches!(
        res_old,
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    assert!(token_old.is_cancelled());

    assert!(manager.close().issues.is_empty());
}

#[test]
fn test_generation_state_sharded_removal_and_task_count() {
    let state = GenerationState::new(1);
    let (abort, _) = AbortHandle::new_pair();
    for id in 1..=100 {
        let index = task_shard(id);
        let (cancellation, _) = CancellationSource::new(CancellationGuarantee::BestEffort);
        state.shards[index].tasks.lock().insert(
            id,
            TaskControl {
                abort: abort.clone(),
                cancellation,
            },
        );
        state.task_count.fetch_add(1, Ordering::AcqRel);
    }

    assert_eq!(state.task_count.load(Ordering::Acquire), 100);

    // Remove 40 tasks via remove_task
    for id in 1..=40 {
        assert!(state.remove_task(id));
    }
    assert_eq!(state.task_count.load(Ordering::Acquire), 60);

    // Drain remaining 60 tasks
    let drained = state.drain_tasks();
    assert_eq!(drained.len(), 60);
    assert_eq!(state.task_count.load(Ordering::Acquire), 0);
}

#[test]
fn spawn_and_advance_linearization_case_a_advance_closes_before_admission() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (snapshot_tx, snapshot_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

    manager.set_after_generation_snapshot_hook(Some(Arc::new(move || {
        snapshot_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("snapshot hook should be released");
    })));

    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        spawn_result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source))
            .unwrap();
    });

    snapshot_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("spawn should snapshot current generation");

    let advancing_manager = Arc::clone(&manager);
    let (advance_result_tx, advance_result_rx) = std::sync::mpsc::sync_channel(1);
    let advancing = std::thread::spawn(move || {
        advance_result_tx
            .send(advancing_manager.advance_generation())
            .unwrap();
    });

    assert!(
        advance_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
    );
    release_tx.send(()).unwrap();

    let spawn_res = spawn_result_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        spawn_res,
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    assert!(token.is_cancelled());

    spawning.join().unwrap();
    advancing.join().unwrap();
    manager.set_after_generation_snapshot_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn spawn_and_advance_linearization_case_b_admission_holds_advance() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

    manager.set_after_generation_admission_hook(Some(Arc::new(move || {
        admitted_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("admission hook should be released");
    })));

    let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        spawn_result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source))
            .unwrap();
    });

    admitted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("spawn should enter admission");

    let advancing_manager = Arc::clone(&manager);
    let (advance_result_tx, advance_result_rx) = std::sync::mpsc::sync_channel(1);
    let advancing = std::thread::spawn(move || {
        advance_result_tx
            .send(advancing_manager.advance_generation())
            .unwrap();
    });

    // advance_generation should block on wait_for_idle while admission is held
    assert!(
        advance_result_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err()
    );

    release_tx.send(()).unwrap();
    assert!(
        spawn_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    assert!(
        advance_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
    );

    spawning.join().unwrap();
    advancing.join().unwrap();
    manager.set_after_generation_admission_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn spawn_and_cancel_linearization_case_a_spawn_admitted_first() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

    manager.set_after_generation_admission_hook(Some(Arc::new(move || {
        admitted_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("admission hook should be released");
    })));

    let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    let spawning_manager = Arc::clone(&manager);
    let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
    let spawning = std::thread::spawn(move || {
        spawn_result_tx
            .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source))
            .unwrap();
    });

    admitted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("spawn should enter admission");

    let cancelling_manager = Arc::clone(&manager);
    let (cancel_result_tx, cancel_result_rx) = std::sync::mpsc::sync_channel(1);
    let cancelling = std::thread::spawn(move || {
        cancelling_manager.cancel_generation(TEST_GENERATION);
        cancel_result_tx.send(()).unwrap();
    });

    // cancel_generation should block waiting for admission idle
    assert!(
        cancel_result_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err()
    );

    release_tx.send(()).unwrap();
    assert!(
        spawn_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    cancel_result_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    spawning.join().unwrap();
    cancelling.join().unwrap();
    assert!(token.is_cancelled());
    manager.set_after_generation_admission_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn spawn_and_cancel_linearization_case_b_cancel_closed_first() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    manager.cancel_generation(TEST_GENERATION);

    let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    let res = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source);
    assert!(matches!(
        res,
        Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
    ));
    assert!(token.is_cancelled());
    assert!(manager.close().issues.is_empty());
}

#[test]
fn advance_does_not_hold_control_mutex_while_waiting_for_idle() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();

    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

    manager.set_after_generation_admission_hook(Some(Arc::new(move || {
        admitted_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("admission hook should be released");
    })));

    let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let spawning = std::thread::spawn(move || {
        spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source)
    });

    admitted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("task should be admitted");

    let advancing_manager = Arc::clone(&manager);
    let (advance_done_tx, advance_done_rx) = std::sync::mpsc::sync_channel(1);
    let advancing = std::thread::spawn(move || {
        advance_done_tx
            .send(advancing_manager.advance_generation())
            .unwrap();
    });

    std::thread::sleep(Duration::from_millis(50));

    let executor_shared = match &*manager.state.lock() {
        ExecutorState::Running(executor) => Arc::clone(&executor.shared),
        _ => panic!("executor should be running"),
    };

    assert!(
        executor_shared.control.try_lock().is_some(),
        "advance_generation must release control mutex while waiting for admission idle"
    );

    release_tx.send(()).unwrap();
    assert!(
        advance_done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
    );

    spawning.join().unwrap().unwrap();
    advancing.join().unwrap();
    manager.set_after_generation_admission_hook(None);
    assert!(manager.close().issues.is_empty());
}

#[test]
fn close_preempts_in_progress_advance_generation() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();

    let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

    manager.set_after_generation_admission_hook(Some(Arc::new(move || {
        admitted_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .expect("admission hook should be released");
    })));

    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let spawning_manager = Arc::clone(&manager);
    let spawning = std::thread::spawn(move || {
        spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source)
    });

    admitted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("task should be admitted");

    let advancing_manager = Arc::clone(&manager);
    let (advance_done_tx, advance_done_rx) = std::sync::mpsc::sync_channel(1);
    let advancing = std::thread::spawn(move || {
        advance_done_tx
            .send(advancing_manager.advance_generation())
            .unwrap();
    });

    std::thread::sleep(Duration::from_millis(50));

    let closing_manager = Arc::clone(&manager);
    let (close_done_tx, close_done_rx) = std::sync::mpsc::sync_channel(1);
    let closing = std::thread::spawn(move || {
        close_done_tx.send(closing_manager.close()).unwrap();
    });

    std::thread::sleep(Duration::from_millis(50));

    let executor_shared = match &*manager.state.lock() {
        ExecutorState::Closing(executor) => executor.as_ref().map(|exec| Arc::clone(&exec.shared)),
        _ => None,
    };

    if let Some(shared) = executor_shared {
        assert!(
            shared.closing.load(Ordering::Acquire),
            "close must set closing atomic mirror even while advance is waiting"
        );
    }

    release_tx.send(()).unwrap();

    assert!(
        !advance_done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
    );

    let close_report = close_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(close_report.issues.is_empty());
    assert!(token.is_cancelled());

    spawning.join().unwrap().unwrap();
    advancing.join().unwrap();
    closing.join().unwrap();
    manager.set_after_generation_admission_hook(None);
}

#[test]
fn async_udf_boundary_catches_unhandled_panics_at_ffi_boundary() {
    struct PanickingLayer;
    struct PanickingGuard;

    impl crate::execution::UdfLayer for PanickingLayer {
        type Guard = PanickingGuard;
        fn enter(&self, _: &CallMetadata) -> XllResult<Self::Guard> {
            panic!("injected layer panic in outer boundary");
        }
    }

    impl crate::execution::UdfLayerGuard for PanickingGuard {
        fn exit(self, _: &CallOutcome<'_>) {}
    }

    struct PanickingAddin;
    impl Addin for PanickingAddin {
        type State = u32;
        type Error = XllError;
        type Layers = (PanickingLayer,);
        fn open(
            _: &OpenContext,
        ) -> Result<crate::addin::Opened<Self::State, Self::Layers>, Self::Error> {
            unreachable!()
        }
    }

    let runtime = Box::leak(Box::new(Runtime::<PanickingAddin>::new()));
    let _guard = test_lock_for_runtime(runtime);
    let mut open_attempt = runtime.begin_open().unwrap();
    runtime.publish(1_u32, (PanickingLayer,));
    runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
    runtime.start_async(1).unwrap();

    let mut bytes = vec![1_u8, 2, 3, 4];
    let mut handle = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: bytes.as_mut_ptr(),
                },
                byte_count: bytes.len() as i32,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };

    // SAFETY: handle is a valid, stack-local XLOPER12 constructed above.
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        async_udf_boundary_named(
            runtime,
            "test_async_panic_boundary",
            "TEST.ASYNC.PANIC",
            &mut handle,
            |_, _| Ok(async { Ok::<_, XllError>(42.0) }),
        );
    }));

    assert!(
        result.is_ok(),
        "async_udf_boundary_named must catch panics at the FFI boundary"
    );
    assert!(runtime.close_async().issues.is_empty());
}

#[test]
fn spawn_fast_path_does_not_wait_for_manager_state() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();

    // Deliberately hold the cold lifecycle mutex.
    let state_guard = manager.state.lock();

    let spawning_manager = Arc::clone(&manager);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);

    let thread = std::thread::spawn(move || {
        let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let result = spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source);
        tx.send(result).unwrap();
    });

    // spawn must complete while `state` remains locked.
    assert!(
        rx.recv_timeout(Duration::from_secs(1))
            .expect("spawn must not wait for manager state")
            .is_ok()
    );

    drop(state_guard);
    thread.join().unwrap();
    assert!(manager.close().issues.is_empty());
}

#[test]
fn stale_spawn_snapshot_rejects_spawns_after_close() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();

    // Take snapshot while running
    let snapshot = manager.snapshot_spawn_executor().unwrap();

    // Close the manager
    let outcome = manager.close();
    assert!(outcome.issues.is_empty());

    // Spawning on stale snapshot must fail with XllError::Closing
    let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let result = snapshot.spawn(TEST_GENERATION, async {}, source);
    assert!(matches!(
        result,
        Err(SpawnRejection {
            error: XllError::Closing,
            ..
        })
    ));
}

#[test]
fn executor_incarnation_identity_guards_advance_generation() {
    let manager = Arc::new(AsyncManager::new());
    manager.start(1).unwrap();
    let old_snapshot = manager.snapshot_spawn_executor().unwrap();

    // Stop and restart to create a new executor incarnation
    assert!(manager.close().issues.is_empty());
    manager.start(1).unwrap();
    let new_snapshot = manager.snapshot_spawn_executor().unwrap();

    // old and new have distinct pointer identities
    assert!(!Arc::ptr_eq(&old_snapshot, &new_snapshot));

    assert!(manager.close().issues.is_empty());
}

#[test]
fn startup_partial_worker_creation_failure_rolls_back_cleanly() {
    // Inject failure at worker index 2 of 4
    let result = Executor::start_with_failure_at(4, 1, Some(2));
    assert!(matches!(
        result,
        Err(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::ASYNC_SPAWN
        })
    ));
}
