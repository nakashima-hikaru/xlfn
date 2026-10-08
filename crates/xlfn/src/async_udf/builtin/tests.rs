use super::*;
use crate::async_udf::future::NoUnwindFuture;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);

fn executor(pollers: usize) -> BuiltinAsyncExecutor {
    BuiltinAsyncExecutor::new(
        BuiltinExecutorConfig::new().with_poller_count(AsyncPollerCount::new(pollers).unwrap()),
    )
}

fn task(
    future: impl Future<Output = ()> + Send + 'static,
) -> AsyncTask<impl Future<Output = ()> + Send + 'static> {
    AsyncTask::new(NoUnwindFuture::new("builtin executor test", future))
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "poller transition did not finish"
        );
        thread::yield_now();
    }
}

struct DropSignal(Arc<AtomicUsize>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

fn pending_task(drops: &Arc<AtomicUsize>) -> AsyncTask<impl Future<Output = ()> + Send + 'static> {
    let drop_signal = DropSignal(Arc::clone(drops));
    task(async move {
        let _drop_signal = drop_signal;
        std::future::pending::<()>().await;
    })
}

#[test]
fn poller_policy_validates_supported_counts() {
    assert!(AsyncPollerCount::new(0).is_none());
    assert_eq!(AsyncPollerCount::new(1).unwrap().get(), 1);
    assert_eq!(AsyncPollerCount::new(32).unwrap().get(), 32);
    assert!(AsyncPollerCount::new(33).is_none());
    assert_eq!(AsyncPollerCount::DEFAULT.get(), 4);
    assert_eq!(
        BuiltinExecutorConfig::new(),
        BuiltinExecutorConfig::default()
    );
}

#[test]
fn executor_starts_once_and_rejects_publication_after_shutdown() {
    let executor = executor(1);
    assert!(executor.reserve().is_err());
    assert!(executor.shared.get().is_none());
    executor.start().unwrap();
    executor.start().unwrap();
    assert_eq!(
        executor
            .shared
            .get()
            .unwrap()
            .live_pollers
            .load(Ordering::Acquire),
        1
    );
    assert!(executor.reserve().is_ok());
    executor.shutdown().unwrap();
    executor.shutdown().unwrap();
    assert!(matches!(executor.start(), Err(XllError::Closing)));
    assert!(matches!(executor.reserve(), Err(XllError::Closing)));
}

#[test]
fn partial_poller_start_failure_rolls_back_before_returning() {
    for fail_at in [0, 2] {
        let executor =
            BuiltinAsyncExecutor::with_start_failure(BuiltinExecutorConfig::new(), fail_at);
        assert!(matches!(executor.start(), Err(XllError::Internal { .. })));
        assert!(executor.shared.get().is_none());
        assert!(executor.reserve().is_err());
        executor.shutdown().unwrap();
    }
}

#[test]
fn miri_reservation_survives_shutdown_and_destroys_late_task_once() {
    let executor = executor(1);
    executor.start().unwrap();
    let reservation = executor.reserve().unwrap();
    let shared = Arc::clone(&reservation.shared);
    executor.shutdown().unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    executor.submit(reservation, pending_task(&drops));
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(shared.live_pollers.load(Ordering::Acquire), 0);
    assert!(shared.queue.is_sealed());
}

#[test]
fn last_poller_panic_drops_queued_tasks_and_retains_arbitrary_payload() {
    let executor = executor(1);
    executor.start().unwrap();
    let shared = Arc::clone(executor.shared.get().unwrap());
    let payload_drops = Arc::new(AtomicUsize::new(0));
    let payload = crate::panic_boundary::tests::PanickingPayload(Arc::clone(&payload_drops));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    executor.submit(
        executor.reserve().unwrap(),
        task(async move {
            entered_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            std::panic::panic_any(payload);
        }),
    );
    entered_rx.recv_timeout(TIMEOUT).unwrap();
    let late_reservation = executor.reserve().unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    for _ in 0..8 {
        executor.submit(executor.reserve().unwrap(), pending_task(&drops));
    }
    resume_tx.send(()).unwrap();
    wait_until(|| drops.load(Ordering::Acquire) == 8);
    assert_eq!(shared.live_pollers.load(Ordering::Acquire), 0);
    assert!(shared.failed.load(Ordering::Acquire));
    assert!(executor.reserve().is_err());
    executor.submit(late_reservation, pending_task(&drops));
    assert_eq!(drops.load(Ordering::Acquire), 9);
    assert!(matches!(executor.shutdown(), Err(XllError::Panic)));
    assert_eq!(payload_drops.load(Ordering::Acquire), 0);
}

#[test]
fn failed_poller_recovers_local_runnables_for_surviving_pollers() {
    let executor = executor(1);
    executor.start().unwrap();
    let shared = Arc::clone(executor.shared.get().unwrap());
    let local = crossbeam_deque::Worker::new_fifo();
    let completed = Arc::new(AtomicUsize::new(0));
    for _ in 0..3 {
        let completed = Arc::clone(&completed);
        let scheduling = Arc::clone(&shared);
        let (runnable, handle) = async_task::spawn(
            async move {
                completed.fetch_add(1, Ordering::Release);
            },
            move |runnable| scheduling.schedule(runnable),
        );
        handle.detach();
        local.push(runnable);
    }
    shared.live_pollers.fetch_add(1, Ordering::Relaxed);
    let failed_shared = Arc::clone(&shared);
    assert!(
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = worker::PollerExitGuard {
                shared: failed_shared,
                local,
            };
            panic!("injected infrastructure poller failure");
        }))
        .is_err()
    );
    wait_until(|| completed.load(Ordering::Acquire) == 3);
    assert!(shared.failed.load(Ordering::Acquire));
    assert!(executor.reserve().is_err());
    executor.shutdown().unwrap();
}

#[test]
fn shutdown_waits_for_scheduler_task_destruction_and_final_callback_release() {
    let executor = Arc::new(executor(1));
    executor.start().unwrap();
    let shared = Arc::clone(executor.shared.get().unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    executor.submit(
        executor.reserve().unwrap(),
        task(async move {
            entered_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            panic!("injected last poller failure");
        }),
    );
    entered_rx.recv_timeout(TIMEOUT).unwrap();
    resume_tx.send(()).unwrap();
    wait_until(|| shared.live_pollers.load(Ordering::Acquire) == 0);
    let drops = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(std::sync::Barrier::new(2));
    let resume = Arc::new(std::sync::Barrier::new(2));
    let entered_hook = Arc::clone(&entered);
    let resume_hook = Arc::clone(&resume);
    *shared.after_scheduler_drop_hook.lock() = Some(Arc::new(move || {
        entered_hook.wait();
        resume_hook.wait();
    }));
    let scheduling = Arc::clone(&shared);
    let (runnable, handle) = async_task::spawn(pending_task(&drops), move |runnable| {
        scheduling.schedule(runnable)
    });
    handle.detach();
    let scheduling = thread::spawn(move || runnable.schedule());
    entered.wait();
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(shared.scheduler_callbacks.active(), 1);
    let closing_executor = Arc::clone(&executor);
    let (closed_tx, closed_rx) = mpsc::channel();
    let closing = thread::spawn(move || closed_tx.send(closing_executor.shutdown()).unwrap());
    wait_until(|| shared.scheduler_callbacks.is_closing());
    assert!(matches!(
        closed_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    resume.wait();
    scheduling.join().unwrap();
    assert!(matches!(
        closed_rx.recv_timeout(TIMEOUT).unwrap(),
        Err(XllError::Panic)
    ));
    closing.join().unwrap();
}

#[test]
fn miri_executor_drop_seals_and_joins_pollers() {
    // Check executor ownership without invoking crossbeam-epoch's global
    // collector. Native pool tests separately exercise peer stealing.
    let executor = executor(1);
    executor.start().unwrap();
    let shared = Arc::clone(executor.shared.get().unwrap());
    drop(executor);
    assert_eq!(shared.live_pollers.load(Ordering::Acquire), 0);
    assert!(shared.queue.is_sealed());
    assert!(shared.scheduler_callbacks.is_closing());
}

#[test]
fn miri_retained_completed_task_waker_can_be_used_after_executor_drop() {
    struct CaptureWaker(Option<mpsc::Sender<std::task::Waker>>);
    impl Future for CaptureWaker {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
            self.0
                .take()
                .unwrap()
                .send(context.waker().clone())
                .unwrap();
            Poll::Ready(())
        }
    }
    let executor = executor(1);
    executor.start().unwrap();
    let (waker_tx, waker_rx) = mpsc::channel();
    executor.submit(
        executor.reserve().unwrap(),
        task(CaptureWaker(Some(waker_tx))),
    );
    let waker = waker_rx.recv_timeout(TIMEOUT).unwrap();
    drop(executor);
    waker.wake();
}

#[test]
fn last_executor_owner_can_be_destroyed_by_its_own_poller() {
    let executor = Arc::new(executor(2));
    executor.start().unwrap();
    let shared = Arc::clone(executor.shared.get().unwrap());
    let owned = Arc::clone(&executor);
    let (resume_tx, resume_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    executor.submit(
        executor.reserve().unwrap(),
        task(async move {
            resume_rx.recv().unwrap();
            drop(owned);
            dropped_tx.send(()).unwrap();
        }),
    );
    drop(executor);
    resume_tx.send(()).unwrap();
    dropped_rx.recv_timeout(TIMEOUT).unwrap();
    wait_until(|| shared.live_pollers.load(Ordering::Acquire) == 0);
    assert!(shared.queue.is_sealed());
}

#[test]
fn last_executor_owner_can_be_destroyed_by_its_scheduler_callback() {
    let executor = Arc::new(executor(1));
    executor.start().unwrap();
    let shared = Arc::clone(executor.shared.get().unwrap());
    executor.submit(
        executor.reserve().unwrap(),
        task(async { panic!("injected poller failure before scheduler drop") }),
    );
    wait_until(|| shared.live_pollers.load(Ordering::Acquire) == 0);
    let (dropped_tx, dropped_rx) = mpsc::channel();
    struct ExecutorOnDrop(Option<Arc<BuiltinAsyncExecutor>>, mpsc::Sender<()>);
    impl Future for ExecutorOnDrop {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
        }
    }
    impl Drop for ExecutorOnDrop {
        fn drop(&mut self) {
            drop(self.0.take());
            self.1.send(()).unwrap();
        }
    }
    let future = ExecutorOnDrop(Some(Arc::clone(&executor)), dropped_tx);
    let scheduling = Arc::clone(&shared);
    let (runnable, handle) =
        async_task::spawn(task(future), move |runnable| scheduling.schedule(runnable));
    handle.detach();
    drop(executor);
    let scheduling = thread::spawn(move || runnable.schedule());
    dropped_rx.recv_timeout(TIMEOUT).unwrap();
    scheduling.join().unwrap();
    assert!(shared.scheduler_callbacks.is_closing());
    assert_eq!(shared.scheduler_callbacks.active(), 0);
}

#[test]
fn rejected_task_destructor_panic_is_contained_and_runs_once() {
    struct PanicDrop(Arc<AtomicUsize>, Arc<AtomicUsize>);
    impl Future for PanicDrop {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
        }
    }
    impl Drop for PanicDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
            std::panic::panic_any(crate::panic_boundary::tests::PanickingPayload(Arc::clone(
                &self.1,
            )));
        }
    }
    let executor = executor(1);
    executor.start().unwrap();
    let reservation = executor.reserve().unwrap();
    executor.shutdown().unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let payload_drops = Arc::new(AtomicUsize::new(0));
    executor.submit(
        reservation,
        task(PanicDrop(Arc::clone(&drops), Arc::clone(&payload_drops))),
    );
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(payload_drops.load(Ordering::Acquire), 0);
}

fn queue_fixture() -> (
    Parker,
    crossbeam_deque::Worker<Runnable>,
    Arc<RunnableQueue>,
) {
    let parker = Parker::new();
    let local = crossbeam_deque::Worker::new_fifo();
    let queue = Arc::new(RunnableQueue::new(
        vec![local.stealer()].into_boxed_slice(),
        vec![parker.unparker().clone()].into_boxed_slice(),
    ));
    (parker, local, queue)
}

fn runnable(
    queue: &Arc<RunnableQueue>,
    ran: &Arc<AtomicBool>,
    drops: &Arc<AtomicUsize>,
) -> Runnable {
    let ran = Arc::clone(ran);
    let signal = DropSignal(Arc::clone(drops));
    let scheduling = Arc::clone(queue);
    let (runnable, handle) = async_task::spawn(
        task(async move {
            let _signal = signal;
            ran.store(true, Ordering::Release);
        }),
        move |runnable| scheduling.schedule(runnable),
    );
    handle.detach();
    runnable
}

#[test]
fn queued_work_before_park_leaves_an_unpark_token() {
    let (parker, local, queue) = queue_fixture();
    queue.announce_idle(1);
    let ran = Arc::new(AtomicBool::new(false));
    let drops = Arc::new(AtomicUsize::new(0));
    queue.schedule(runnable(&queue, &ran, &drops));
    assert_eq!(queue.idle_pollers.load(Ordering::Acquire), 0);
    // Bounded wait also detects a lost token without hanging the test runner.
    let started = Instant::now();
    parker.park_timeout(TIMEOUT);
    assert!(
        started.elapsed() < TIMEOUT / 2,
        "queued work lost its unpark token"
    );
    queue.steal_injector_batch_and_pop(&local).unwrap().run();
    assert!(ran.load(Ordering::Acquire));
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

#[test]
fn work_published_before_idle_announcement_is_found_by_recheck() {
    let (_parker, local, queue) = queue_fixture();
    let ran = Arc::new(AtomicBool::new(false));
    let drops = Arc::new(AtomicUsize::new(0));
    queue.schedule(runnable(&queue, &ran, &drops));
    queue.announce_idle(1);
    queue.steal_injector_batch_and_pop(&local).unwrap().run();
    assert!(ran.load(Ordering::Acquire));
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

#[test]
fn queue_seal_rejects_and_destroys_new_runnables() {
    let (_parker, local, queue) = queue_fixture();
    let ran_before = Arc::new(AtomicBool::new(false));
    let ran_after = Arc::new(AtomicBool::new(false));
    let drops = Arc::new(AtomicUsize::new(0));
    queue.schedule(runnable(&queue, &ran_before, &drops));
    queue.seal_and_wake_all();
    queue.schedule(runnable(&queue, &ran_after, &drops));
    assert_eq!(drops.load(Ordering::Acquire), 1);
    queue.steal_injector_batch_and_pop(&local).unwrap().run();
    assert!(queue.steal_injector_batch_and_pop(&local).is_none());
    assert!(ran_before.load(Ordering::Acquire));
    assert!(!ran_after.load(Ordering::Acquire));
    assert_eq!(drops.load(Ordering::Acquire), 2);
}

#[test]
fn poller_can_steal_opaque_runnable_from_peer_queue() {
    let local = crossbeam_deque::Worker::new_fifo();
    let peer = crossbeam_deque::Worker::new_fifo();
    let parkers = [Parker::new(), Parker::new()];
    let queue = Arc::new(RunnableQueue::new(
        vec![local.stealer(), peer.stealer()].into_boxed_slice(),
        parkers
            .iter()
            .map(|parker| parker.unparker().clone())
            .collect(),
    ));
    let ran = Arc::new(AtomicBool::new(false));
    let drops = Arc::new(AtomicUsize::new(0));
    peer.push(runnable(&queue, &ran, &drops));
    queue.steal_peer(0).unwrap().run();
    assert!(ran.load(Ordering::Acquire));
    assert!(peer.is_empty());
    assert_eq!(drops.load(Ordering::Acquire), 1);
}
