use super::executor::{ExecutorPtr, ExecutorShared};
use super::future::NoUnwindFuture;
use super::generation::GenerationPin;
use super::manager::MAX_PENDING;
use super::worker::release_active;
use crate::cancellation::CancellationSource;
use futures_util::future::{AbortHandle, AbortRegistration, Abortable};
use smallvec::SmallVec;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

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
    shared: Option<&'a ExecutorShared>,
}

impl<'a> ActiveReservation<'a> {
    pub(crate) fn try_acquire(shared: &'a ExecutorShared) -> Option<Self> {
        // Generation admission protects lifecycle entry. This RMW only
        // reserves capacity; completion releases publish work to the drainer.
        shared
            .active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                (active < MAX_PENDING).then_some(active + 1)
            })
            .ok()?;

        Some(Self {
            shared: Some(shared),
        })
    }

    pub(crate) fn commit(mut self, generation: GenerationPin, id: u64) -> CompletionGuard {
        let shared = self.shared.take().expect("reservation owns active count");

        CompletionGuard {
            shared: ExecutorPtr::from_ref(shared),
            generation: Some(generation),
            id,
            observation: CompletionObservation::new(),
        }
    }
}

impl<'a> Drop for ActiveReservation<'a> {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            release_active(shared);
        }
    }
}

pub(crate) struct CompletionGuard {
    pub(crate) shared: ExecutorPtr,
    pub(crate) generation: Option<GenerationPin>,
    pub(crate) id: u64,
    pub(crate) observation: CompletionObservation,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        let generation = self
            .generation
            .take()
            .expect("completion owns its generation pin");
        // SAFETY: task destruction runs under a scheduler callback permit,
        // a joined worker, or the caller's executor publication admission.
        let shared = unsafe { self.shared.get() };
        generation.get().remove_task(self.id);
        shared
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::EndAsyncTask(
                self.observation.completion(),
            ));
        // Release the generation before active. Final shutdown additionally
        // drains scheduler callbacks and joins workers before reclamation.
        drop(generation);
        release_active(shared);
    }
}

// SAFETY: the pointed-to states are Sync; task counts, scheduler callback
// admission, and worker joins collectively retain their unique owners.
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

#[cfg(test)]
mod reservation_tests {
    use super::*;
    use crate::async_udf::executor::{Executor, SpawnReservation};

    #[test]
    fn miri_reservation_transfers_or_releases_exactly_one_active_count() {
        assert_eq!(
            size_of::<ActiveReservation<'_>>(),
            size_of::<&ExecutorShared>()
        );
        let executor = Executor::start(1, 1).unwrap();
        let shared = &*executor.shared;
        let reservation = shared.reserve_spawn(1).unwrap();
        assert_eq!(shared.active.load(Ordering::Relaxed), 1);
        drop(reservation);
        assert_eq!(shared.active.load(Ordering::Relaxed), 0);

        let SpawnReservation {
            admission,
            generation,
            task_id,
            reservation,
            ..
        } = shared.reserve_spawn(1).unwrap();
        let completion = reservation.commit(generation, task_id);
        drop(admission);
        assert_eq!(shared.active.load(Ordering::Relaxed), 1);
        drop(completion);
        assert_eq!(shared.active.load(Ordering::Relaxed), 0);
    }
}
