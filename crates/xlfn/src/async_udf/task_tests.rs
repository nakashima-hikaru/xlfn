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
    cancel_tasks(registry.request_close());
    registry.finish_close();
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
