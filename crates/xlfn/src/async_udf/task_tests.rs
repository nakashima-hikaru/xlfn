use super::calculation::CalculationEpoch;
use super::registry::{TaskRegistry, cancel_tasks};
use crate::AsyncTaskLimit;
use crate::cancellation::{CancellationGuarantee, CancellationSource};
use std::future::Future;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};

#[derive(Default)]
struct Observation {
    polls: AtomicUsize,
    drops: AtomicUsize,
    invalid_drop: AtomicBool,
}

struct PinnedFuture {
    address: AtomicUsize,
    observation: Arc<Observation>,
    registry: Arc<TaskRegistry>,
    panic_on_drop: bool,
    _pin: PhantomPinned,
}

impl Future for PinnedFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        let address = std::ptr::from_ref(&*self).addr();
        let previous = self.address.swap(address, Ordering::Relaxed);
        assert!(previous == 0 || previous == address);
        assert_eq!(self.registry.active_tasks.load(Ordering::Acquire), 1);
        if self.observation.polls.fetch_add(1, Ordering::Relaxed) == 0 {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

impl Drop for PinnedFuture {
    fn drop(&mut self) {
        let address = self.address.load(Ordering::Relaxed);
        let invalid = (address != 0 && address != std::ptr::from_ref(self).addr())
            || self.registry.active_tasks.load(Ordering::Acquire) != 1;
        // The task intentionally contains destructor panics, so report failed
        // lifetime checks externally instead of letting that boundary hide them.
        self.observation
            .invalid_drop
            .store(invalid, Ordering::Relaxed);
        self.observation.drops.fetch_add(1, Ordering::Relaxed);
        if self.panic_on_drop {
            panic!("injected inline task destructor panic");
        }
    }
}

#[derive(Clone, Copy)]
enum Completion {
    PendingDrop,
    Ready,
    Cancelled,
}

fn registry() -> Arc<TaskRegistry> {
    let registry = Arc::new(TaskRegistry::new());
    registry.start(AsyncTaskLimit::DEFAULT).unwrap();
    registry
}

fn source() -> CancellationSource {
    CancellationSource::new(CancellationGuarantee::CalculationScoped).0
}

fn future(
    registry: &Arc<TaskRegistry>,
    observation: &Arc<Observation>,
    panic_on_drop: bool,
) -> PinnedFuture {
    PinnedFuture {
        address: AtomicUsize::new(0),
        observation: Arc::clone(observation),
        registry: Arc::clone(registry),
        panic_on_drop,
        _pin: PhantomPinned,
    }
}

fn assert_released(registry: &TaskRegistry, observation: &Observation) {
    assert_eq!(observation.drops.load(Ordering::Relaxed), 1);
    assert!(!observation.invalid_drop.load(Ordering::Relaxed));
    assert_eq!(registry.active_tasks.load(Ordering::Acquire), 0);
    registry.request_close().unwrap().cancel().wait().finish();
}

#[test]
fn miri_inline_async_task_preserves_non_unpin_future_until_final_drop() {
    for completion in [
        Completion::PendingDrop,
        Completion::Ready,
        Completion::Cancelled,
    ] {
        for panic_on_drop in [false, true] {
            let registry = registry();
            let observation = Arc::new(Observation::default());
            let task = registry
                .reserve(CalculationEpoch::INITIAL)
                .unwrap()
                .commit(future(&registry, &observation, panic_on_drop), source());
            // Ownership transfer may move a task before its executor pins it.
            let task = std::convert::identity(task);
            let mut task = Box::pin(task);
            let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
            assert!(task.as_mut().poll(&mut context).is_pending());
            match completion {
                Completion::PendingDrop => {}
                Completion::Ready => {
                    assert!(task.as_mut().poll(&mut context).is_ready());
                    assert_eq!(observation.polls.load(Ordering::Relaxed), 2);
                }
                Completion::Cancelled => {
                    cancel_tasks(registry.cancel_calculation(CalculationEpoch::INITIAL));
                    assert!(task.as_mut().poll(&mut context).is_ready());
                    assert_eq!(observation.polls.load(Ordering::Relaxed), 1);
                }
            }
            // Ready is not a task-lifetime certificate: an executor may keep
            // the completed task, including its user captures, until Drop.
            assert_eq!(registry.active_tasks.load(Ordering::Acquire), 1);
            assert_eq!(observation.drops.load(Ordering::Relaxed), 0);
            drop(task);
            assert_released(&registry, &observation);
        }
    }
}

#[test]
fn miri_inline_async_task_drops_unpolled_future_before_registry_release() {
    for panic_on_drop in [false, true] {
        let registry = registry();
        let observation = Arc::new(Observation::default());
        let task = registry
            .reserve(CalculationEpoch::INITIAL)
            .unwrap()
            .commit(future(&registry, &observation, panic_on_drop), source());
        drop(task);
        assert_eq!(observation.polls.load(Ordering::Relaxed), 0);
        assert_released(&registry, &observation);
    }
}

#[test]
fn miri_task_cancel_during_user_poll_cannot_miss_registration() {
    struct CancelDuringPoll(Arc<TaskRegistry>, Arc<AtomicUsize>);
    impl Future for CancelDuringPoll {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            self.1.fetch_add(1, Ordering::Relaxed);
            cancel_tasks(self.0.cancel_calculation(CalculationEpoch::INITIAL));
            Poll::Pending
        }
    }
    let registry = registry();
    let polls = Arc::new(AtomicUsize::new(0));
    let task = registry.reserve(CalculationEpoch::INITIAL).unwrap().commit(
        CancelDuringPoll(Arc::clone(&registry), Arc::clone(&polls)),
        source(),
    );
    let mut task = Box::pin(task);
    assert!(
        task.as_mut()
            .poll(&mut Context::from_waker(
                futures_util::task::noop_waker_ref()
            ))
            .is_ready()
    );
    assert_eq!(polls.load(Ordering::Relaxed), 1);
    // Completed task retention still retains the calculation/registry authority.
    assert_eq!(registry.active_tasks.load(Ordering::Acquire), 1);
    drop(task);
    assert_eq!(registry.active_tasks.load(Ordering::Acquire), 0);
    registry.request_close().unwrap().cancel().wait().finish();
}

#[test]
fn task_waker_destructor_panic_cannot_skip_task_accounting_release() {
    struct PanicDrop(Arc<AtomicUsize>);
    impl std::task::Wake for PanicDrop {
        fn wake(self: Arc<Self>) {
            // This fixture exercises destruction without cancellation. Record
            // an unexpected wake separately from its single destructor count.
            self.0.fetch_add(100, Ordering::Relaxed);
        }
    }
    impl Drop for PanicDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
            panic!("injected task waker destructor panic");
        }
    }
    let registry = registry();
    let drops = Arc::new(AtomicUsize::new(0));
    let task = registry
        .reserve(CalculationEpoch::INITIAL)
        .unwrap()
        .commit(std::future::pending::<()>(), source());
    let mut task = Box::pin(task);
    let waker = std::task::Waker::from(Arc::new(PanicDrop(Arc::clone(&drops))));
    assert!(
        task.as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    drop(waker);
    // Task registration releases its waker before completion removes the
    // source. Its arbitrary destructor panic must remain contained.
    drop(task);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(registry.active_tasks.load(Ordering::Acquire), 0);
    registry.request_close().unwrap().cancel().wait().finish();
}

#[test]
fn miri_registry_close_drop_cancels_and_allows_only_close_retry() {
    let registry = registry();
    let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    let task = registry
        .reserve(CalculationEpoch::INITIAL)
        .unwrap()
        .commit(std::future::pending::<()>(), source);

    let closing = registry.request_close().unwrap();
    assert!(matches!(
        registry.request_close(),
        Err(crate::XllError::Closing)
    ));
    assert!(matches!(
        registry.start(AsyncTaskLimit::DEFAULT),
        Err(crate::XllError::Closing)
    ));
    assert!(!token.is_cancelled());
    // Dropping the pending stage performs its cancellation obligation. It
    // releases the close attempt but cannot grant permission to restart.
    drop(closing);
    assert!(token.is_cancelled());
    assert_eq!(registry.active_tasks.load(Ordering::Acquire), 1);
    assert!(matches!(
        registry.start(AsyncTaskLimit::DEFAULT),
        Err(crate::XllError::Closing)
    ));
    drop(task);

    let drained = registry.request_close().unwrap().cancel().wait();
    assert!(matches!(
        registry.request_close(),
        Err(crate::XllError::Closing)
    ));
    // Even a completed wait requires the final, consuming state transition.
    drop(drained);
    assert!(matches!(
        registry.start(AsyncTaskLimit::DEFAULT),
        Err(crate::XllError::Closing)
    ));
    registry.request_close().unwrap().cancel().wait().finish();
    registry.start(AsyncTaskLimit::DEFAULT).unwrap();
    registry.request_close().unwrap().cancel().wait().finish();
}

#[test]
fn miri_registry_close_wait_retains_task_and_release_tail_authority() {
    let registry = registry();
    let task = registry
        .reserve(CalculationEpoch::INITIAL)
        .unwrap()
        .commit(std::future::pending::<()>(), source());
    let closing = registry.request_close().unwrap().cancel();
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(move || {
            waiting_tx.send(()).unwrap();
            closing.wait().finish();
            finished_tx.send(()).unwrap();
        });
        waiting_rx.recv().unwrap();
        assert!(matches!(
            finished_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            registry.start(AsyncTaskLimit::DEFAULT),
            Err(crate::XllError::Closing)
        ));
        drop(task);
        finished_rx.recv().unwrap();
        waiter.join().unwrap();
    });
    registry.start(AsyncTaskLimit::DEFAULT).unwrap();
    registry.request_close().unwrap().cancel().wait().finish();
}

#[test]
fn registry_close_cancellation_callbacks_run_outside_control_lock() {
    struct ReentrantWake(Arc<TaskRegistry>, Arc<AtomicUsize>);
    impl std::task::Wake for ReentrantWake {
        fn wake(self: Arc<Self>) {
            assert!(self.0.control.try_lock().is_some());
            assert!(matches!(
                self.0.request_close(),
                Err(crate::XllError::Closing)
            ));
            assert!(matches!(
                self.0.start(AsyncTaskLimit::DEFAULT),
                Err(crate::XllError::Closing)
            ));
            assert!(!self.0.advance());
            self.1.fetch_add(1, Ordering::Relaxed);
        }
    }
    let registry = registry();
    let wakes = Arc::new(AtomicUsize::new(0));
    let mut task = Box::pin(
        registry
            .reserve(CalculationEpoch::INITIAL)
            .unwrap()
            .commit(std::future::pending::<()>(), source()),
    );
    let waker = std::task::Waker::from(Arc::new(ReentrantWake(
        Arc::clone(&registry),
        Arc::clone(&wakes),
    )));
    assert!(
        task.as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    let closing = registry.request_close().unwrap().cancel();
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    drop(task);
    closing.wait().finish();
}
