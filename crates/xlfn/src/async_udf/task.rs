use super::calculation::CalculationPin;
use super::future::NoUnwindFuture;
use super::registry::{RegistryPtr, TaskRegistry};

use crate::cancellation::CancellationSource;
use futures_util::future::{AbortHandle, AbortRegistration, Abortable};
use smallvec::SmallVec;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

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
    future: NoUnwindFuture<Abortable<F>>,
    completion: CompletionGuard,
}

impl<F> TrackedFuture<F> {
    pub(crate) fn new(
        future: F,
        registration: AbortRegistration,
        completion: CompletionGuard,
    ) -> Self {
        Self {
            future: NoUnwindFuture::new(
                "async task destruction",
                Abortable::new(future, registration),
            ),
            completion,
        }
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
    pub(crate) abort: AbortHandle,
    pub(crate) cancellation: CancellationSource,
}

/// Temporary cancellation ownership, consumed after executor locks are released.
pub(crate) type TaskControlBatch = SmallVec<[TaskControl; 4]>;

pub(crate) struct ActiveReservation<'a> {
    registry: Option<&'a TaskRegistry>,
}

impl<'a> ActiveReservation<'a> {
    pub(crate) fn try_acquire(registry: &'a TaskRegistry) -> Option<Self> {
        // This tail permit protects notification after the authority reaches
        // zero. It never substitutes for active_tasks in the shutdown proof.
        registry.activity.try_acquire().ok()?;
        if registry
            .active_tasks
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                (active < registry.capacity()).then(|| active + 1)
            })
            .is_err()
        {
            registry.activity.release();
            return None;
        }
        Some(Self {
            registry: Some(registry),
        })
    }

    pub(crate) fn commit(mut self, calculation: CalculationPin, id: u64) -> CompletionGuard {
        let registry = self.registry.take().expect("reservation owns active count");
        CompletionGuard {
            registry: RegistryPtr::from_ref(registry),
            calculation: Some(calculation),
            id,
            observation: CompletionObservation::new(),
        }
    }
}

impl Drop for ActiveReservation<'_> {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.take() {
            release_active(registry);
        }
    }
}

// The production and Loom release protocol share this ordering. The extra
// tail lease protects notification after active_tasks publishes zero; it is
// never an alternative task-lifetime certificate.
macro_rules! release_task_activity {
    ($decrement:expr, $notify:expr, $release_tail:expr) => {{
        let previous = $decrement;
        if previous == 1 {
            $notify;
        }
        $release_tail;
    }};
}

fn release_active(registry: &TaskRegistry) {
    release_task_activity!(
        xlfn_kernel::invariant::checked_atomic_dec_release(&registry.active_tasks),
        {
            let _lock = registry.idle_lock.lock();
            registry.idle.notify_all();
        },
        // Final registry access. Its owner waits the authority count and this
        // notification tail before reclaiming the stable allocation.
        registry.activity.release()
    );
}

pub(crate) struct CompletionGuard {
    registry: RegistryPtr,
    calculation: Option<CalculationPin>,
    id: u64,
    observation: CompletionObservation,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        let calculation = self
            .calculation
            .take()
            .expect("completion owns calculation pin");
        // SAFETY: active_tasks retains the registry, with activity protecting
        // the terminal notification tail after the last authority release.
        let registry = unsafe { self.registry.get() };
        calculation.get().remove_task(self.id);
        registry
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::EndAsyncTask(
                self.observation.completion(),
            ));
        drop(calculation);
        release_active(registry);
    }
}

// SAFETY: synchronized states are retained by task accounting and calculation
// pins. No property of any executor participates in this lifetime proof.
unsafe impl Send for CompletionGuard {}

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
                release_task_activity!(
                    finishing.active_tasks.fetch_sub(1, Ordering::Release),
                    {
                        let _guard = finishing.idle.0.lock().unwrap();
                        assert!(!finishing.reclaimed.load(Ordering::Acquire));
                        finishing.idle.1.notify_all();
                    },
                    {
                        // Model the kernel DrainGate release-tail grace period:
                        // it retains its final count through notification.
                        let _guard = finishing.tail_idle.0.lock().unwrap();
                        assert!(!finishing.reclaimed.load(Ordering::Acquire));
                        finishing.release_tail.store(0, Ordering::Release);
                        finishing.tail_idle.1.notify_all();
                    }
                );
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
