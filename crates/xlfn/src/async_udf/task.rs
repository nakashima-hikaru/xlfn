use super::executor::{ExecutorPtr, ExecutorShared};
use super::generation::GenerationState;
use super::manager::MAX_PENDING;
use super::worker::release_active;
use crate::cancellation::CancellationSource;
use crate::error::XllError;
use futures_util::future::AbortHandle;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;

pub(crate) struct TaskControl {
    pub(crate) abort: AbortHandle,
    pub(crate) cancellation: CancellationSource,
}

pub(crate) struct SpawnRejection<F> {
    pub(crate) error: XllError,
    pub(crate) future: F,
    pub(crate) cancellation: CancellationSource,
    pub(crate) cancel: bool,
}

pub(crate) struct ActiveReservation<'a> {
    pub(crate) shared: &'a ExecutorShared,
    pub(crate) armed: bool,
}

impl<'a> ActiveReservation<'a> {
    pub(crate) fn try_acquire(shared: &'a ExecutorShared) -> Option<Self> {
        shared
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_PENDING).then_some(active + 1)
            })
            .ok()?;

        Some(Self {
            shared,
            armed: true,
        })
    }

    pub(crate) fn commit(
        mut self,
        shared: &ExecutorShared,
        generation: &GenerationState,
        id: u64,
    ) -> CompletionGuard {
        self.armed = false;

        CompletionGuard {
            shared: ExecutorPtr::from_ref(shared),
            generation: NonNull::from(generation),
            id,
            observation: CompletionObservation::new(),
        }
    }
}

impl<'a> Drop for ActiveReservation<'a> {
    fn drop(&mut self) {
        if self.armed {
            release_active(self.shared);
        }
    }
}

pub(crate) struct CompletionGuard {
    pub(crate) shared: ExecutorPtr,
    pub(crate) generation: NonNull<GenerationState>,
    pub(crate) id: u64,
    pub(crate) observation: CompletionObservation,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        // SAFETY: this completion contributes to both the generation task
        // count and executor active count. Reclamation waits for those counts
        // to drain, so both pointers remain valid through this Drop.
        let generation = unsafe { self.generation.as_ref() };
        let shared = self.shared.get();
        generation.remove_task(self.id);
        shared
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::EndAsyncTask(
                self.observation.completion(),
            ));
        release_active(shared);
    }
}

// SAFETY: the pointed-to states are Sync, and their unique owners defer
// reclamation until this completion guard releases the tracked task counts.
unsafe impl Send for CompletionGuard {}

pub(crate) struct CompletionObservation {
    #[cfg(any(test, feature = "refinement"))]
    completion: parking_lot::Mutex<crate::shutdown_trace::Completion>,
}

impl CompletionObservation {
    fn new() -> Self {
        Self {
            #[cfg(any(test, feature = "refinement"))]
            completion: parking_lot::Mutex::new(crate::shutdown_trace::Completion::Failed),
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
