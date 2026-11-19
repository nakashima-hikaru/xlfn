use super::calculation::CalculationPin;
use super::future::{NoUnwindFuture, TaskDestruction};
use super::registry::{RegistryPtr, TaskRegistry};

use crate::cancellation::{TaskCancellationAuthority, TaskCancellationRegistration};
use smallvec::SmallVec;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use xlfn_kernel::drain_gate::DetachedDrainPermit;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

/// Opaque framework task transferred to an application-selected executor.
///
/// Only xlfn constructs these tasks. Polling and destroying them completes
/// framework accounting; executors never receive calculation or Excel state.
/// The future stays inline so an executor can place it directly in its task
/// allocation without an intermediate framework allocation.
#[must_use = "async tasks must be polled or dropped to release framework ownership"]
pub struct AsyncTask<F> {
    inner: F,
}

impl<F: Future<Output = ()> + Send + 'static> AsyncTask<F> {
    pub(crate) fn new(future: F) -> Self {
        Self { inner: future }
    }
}

impl<F: Future<Output = ()>> Future for AsyncTask<F> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // SAFETY: pinning this wrapper pins its inline future. No method exposes
        // or moves that field, and field destruction drops it in place.
        unsafe { self.map_unchecked_mut(|task| &mut task.inner) }.poll(cx)
    }
}

/// Owns task destruction order independently of compiler-generated async
/// captures. Even before the first poll, user cleanup precedes the release
/// that permits generation and executor reclamation.
pub(crate) struct TrackedFuture<F> {
    future: NoUnwindFuture<CancellableTaskFuture<F>, TaskDestruction>,
    completion: CompletionGuard,
}

impl<F> TrackedFuture<F> {
    pub(crate) fn new(
        future: F,
        registration: TaskCancellationRegistration,
        completion: CompletionGuard,
    ) -> Self {
        Self {
            future: NoUnwindFuture::new(
                TaskDestruction,
                CancellableTaskFuture {
                    future,
                    registration,
                },
            ),
            completion,
        }
    }
}

// Field order matters: user destruction precedes unregistering its task waker,
// and both finish before TrackedFuture releases its completion/source/pins.
struct CancellableTaskFuture<F> {
    future: F,
    registration: TaskCancellationRegistration,
}

impl<F: Future<Output = ()>> Future for CancellableTaskFuture<F> {
    type Output = Result<(), ()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: the user future stays at this address until in-place drop;
        // registration is not structurally pinned and never exposes that field.
        let this = unsafe { self.get_unchecked_mut() };
        if this.registration.is_cancelled() {
            return Poll::Ready(Err(()));
        }
        // SAFETY: the enclosing pin protects the field for every poll and drop.
        if unsafe { Pin::new_unchecked(&mut this.future) }
            .poll(context)
            .is_ready()
        {
            return Poll::Ready(Ok(()));
        }
        // Registration rechecks cancellation after publishing the waker. A
        // cancellation during the user poll cannot disappear without a wake.
        this.registration.poll_cancelled(context).map(|()| Err(()))
    }
}

impl<F: Future<Output = ()>> Future for TrackedFuture<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        // SAFETY: the inline future is never moved after this wrapper is pinned;
        // its field is destroyed before the completion guard releases ownership.
        let this = unsafe { self.get_unchecked_mut() };
        // SAFETY: the enclosing pin protects this field after this reborrow
        // ends; no method moves it and field drop destroys it in place before
        // completion. The &mut pointer upholds PinSafePointer's contract.
        match unsafe { Pin::new_unchecked(&mut this.future) }.poll(context) {
            Poll::Ready(result) => {
                this.completion.observation.finished(result.is_ok());
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(crate) struct TaskControl {
    pub(crate) cancellation: TaskCancellationAuthority,
}

/// Temporary cancellation ownership, consumed after executor locks are released.
pub(crate) type TaskControlBatch = SmallVec<[TaskControl; 4]>;

pub(crate) struct ActiveReservation<'a> {
    activity: RollbackGuard<ReservedRegistryActivity<'a>>,
}

impl<'a> ActiveReservation<'a> {
    pub(crate) fn try_acquire(registry: &'a TaskRegistry) -> crate::XllResult<Self> {
        // SAFETY: this reservation borrows the registry. Once committed, close
        // waits for both task accounting and the notification tail before
        // reclaiming the allocation.
        let tail = unsafe { registry.activity.try_enter_detached() }
            .map_err(|_| crate::XllError::Closing)?;
        registry
            .active_tasks
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                (active < registry.capacity()).then(|| active + 1)
            })
            .map_err(|_| crate::XllError::Overloaded)?;
        Ok(Self {
            activity: RollbackGuard::new(ReservedRegistryActivity { registry, tail }),
        })
    }

    pub(crate) fn commit(self, calculation: CalculationPin, id: u64) -> CompletionGuard {
        CompletionGuard {
            calculation,
            activity: self.activity.complete().into_activity(),
            id,
            observation: CompletionObservation::new(),
        }
    }
}

/// Keeps the registry borrowed through reservation rollback or transfer.
struct ReservedRegistryActivity<'a> {
    registry: &'a TaskRegistry,
    tail: DetachedDrainPermit,
}

impl ReservedRegistryActivity<'_> {
    fn into_activity(self) -> RegistryActivity {
        RegistryActivity {
            registry: RegistryPtr::from_ref(self.registry),
            _tail: self.tail,
        }
    }
}

impl Rollback for ReservedRegistryActivity<'_> {
    fn rollback(self) {
        drop(self.into_activity());
    }
}

// The production and Loom release protocol share the active count update and
// notification. Their notification-tail permits are released afterwards.
macro_rules! release_task_activity {
    ($decrement:expr, $notify:expr) => {{
        let previous = $decrement;
        if previous == 1 {
            $notify;
        }
    }};
}

/// One reserved task slot and its matching notification-tail permit.
struct RegistryActivity {
    registry: RegistryPtr,
    // Drop glue releases this after the count update and notification.
    _tail: DetachedDrainPermit,
}

impl Drop for RegistryActivity {
    fn drop(&mut self) {
        release_task_activity!(
            {
                // SAFETY: the tail permit retains the exact registry through
                // the count update and subsequent notification.
                let registry = unsafe { self.registry.get() };
                xlfn_kernel::invariant::checked_atomic_dec_release(&registry.active_tasks)
            },
            {
                // SAFETY: the tail permit is released after this block unlocks.
                let registry = unsafe { self.registry.get() };
                let _lock = registry.idle_lock.lock();
                registry.idle.notify_all();
            }
        );
        // The permit's destructor is the final registry access. All registry
        // references and notification guards have gone out of scope by then.
    }
}

pub(crate) struct CompletionGuard {
    // After removing the task and recording its completion, drop the pin
    // before releasing registry activity, including during unwinding.
    calculation: CalculationPin,
    activity: RegistryActivity,
    id: u64,
    observation: CompletionObservation,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.calculation.get().remove_task(self.id);
        // SAFETY: active_tasks and the exact RegistryActivity pair retain the
        // registry through observer updates, before final activity release.
        let registry = unsafe { self.activity.registry.get() };
        registry
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::EndAsyncTask(
                self.observation.completion(),
            ));
    }
}

pub(crate) struct CompletionObservation {
    #[cfg(any(test, feature = "refinement"))]
    completion: crate::sync::Mutex<crate::shutdown_trace::Completion>,
}

impl CompletionObservation {
    fn new() -> Self {
        Self {
            #[cfg(any(test, feature = "refinement"))]
            completion: crate::sync::Mutex::new(crate::shutdown_trace::Completion::Failed),
        }
    }

    pub(crate) fn finished(&self, completed: bool) {
        #[cfg(any(test, feature = "refinement"))]
        {
            *self.completion.lock() = if completed {
                crate::shutdown_trace::Completion::Completed
            } else {
                crate::shutdown_trace::Completion::Canceled
            };
        }
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = completed;
    }

    fn completion(&self) -> crate::shutdown_trace::Completion {
        #[cfg(any(test, feature = "refinement"))]
        {
            return *self.completion.lock();
        }
        #[cfg(not(any(test, feature = "refinement")))]
        crate::shutdown_trace::Completion::Completed
    }
}

#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86"))))]
mod loom_tests {
    use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use loom::sync::{Arc, Condvar, Mutex};
    use loom::thread;

    struct Registry {
        active_tasks: AtomicUsize,
        release_tail: AtomicUsize,
        idle: (Mutex<()>, Condvar),
        tail_idle: (Mutex<()>, Condvar),
        cleanup: AtomicUsize,
        reclaimed: AtomicBool,
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_registry_authority_and_notification_tail_precede_reclamation() {
        loom::model(|| {
            let registry = Arc::new(Registry {
                active_tasks: AtomicUsize::new(1),
                release_tail: AtomicUsize::new(1),
                idle: (Mutex::new(()), Condvar::new()),
                tail_idle: (Mutex::new(()), Condvar::new()),
                cleanup: AtomicUsize::new(0),
                reclaimed: AtomicBool::new(false),
            });
            let finishing = Arc::clone(&registry);
            let task = thread::spawn(move || {
                finishing.cleanup.store(7, Ordering::Relaxed);
                release_task_activity!(finishing.active_tasks.fetch_sub(1, Ordering::Release), {
                    let _guard = finishing.idle.0.lock().unwrap();
                    assert!(!finishing.reclaimed.load(Ordering::Acquire));
                    finishing.idle.1.notify_all();
                });
                // Model the kernel DrainGate release-tail grace period:
                // it retains its final count through notification.
                let _guard = finishing.tail_idle.0.lock().unwrap();
                assert!(!finishing.reclaimed.load(Ordering::Acquire));
                finishing.release_tail.store(0, Ordering::Release);
                finishing.tail_idle.1.notify_all();
            });
            let mut idle = registry.idle.0.lock().unwrap();
            while registry.active_tasks.load(Ordering::Acquire) != 0 {
                idle = registry.idle.1.wait(idle).unwrap();
            }
            drop(idle);
            assert_eq!(registry.cleanup.load(Ordering::Relaxed), 7);
            let mut tail = registry.tail_idle.0.lock().unwrap();
            while registry.release_tail.load(Ordering::Acquire) != 0 {
                tail = registry.tail_idle.1.wait(tail).unwrap();
            }
            registry.reclaimed.store(true, Ordering::Release);
            drop(tail);
            task.join().unwrap();
        });
    }
}

#[cfg(test)]
mod release_owner_tests {
    use super::*;
    use crate::cancellation::CancellationSource;
    use xlfn_kernel::published_owner::PublishedOwner;

    #[test]
    fn miri_registry_final_tail_allows_immediate_owner_reclamation() {
        for _ in 0..16 {
            let owner = PublishedOwner::new(TaskRegistry::new());
            owner
                .start(crate::addin_impl::AsyncTaskLimit::new(1).unwrap())
                .unwrap();
            let reservation = owner.reserve(owner.current_epoch()).unwrap();
            let (cancellation, _token) =
                CancellationSource::new(crate::cancellation::CancellationGuarantee::BestEffort);
            let task = reservation.commit(std::future::ready(()), cancellation);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let dropping = std::sync::Arc::clone(&barrier);
            let worker = std::thread::spawn(move || {
                dropping.wait();
                drop(task);
            });
            let closing = owner.request_close().unwrap().cancel();
            barrier.wait();
            closing.wait().finish();
            // Joining the worker must not be the hidden lifetime barrier.
            // Close's authority and final notification tail are sufficient.
            drop(owner);
            worker.join().unwrap();
        }
    }
}
