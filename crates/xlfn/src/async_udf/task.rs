use super::executor::{ExecutorPtr, ExecutorShared};
use super::generation::GenerationPin;
use super::manager::MAX_PENDING;
use super::worker::release_active;
use crate::cancellation::CancellationSource;
use futures_util::future::AbortHandle;
use std::sync::atomic::Ordering;

pub(crate) struct TaskControl {
    pub(crate) abort: AbortHandle,
    pub(crate) cancellation: CancellationSource,
}

pub(crate) struct ActiveReservation<'a> {
    pub(crate) shared: &'a ExecutorShared,
    pub(crate) armed: bool,
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
            shared,
            armed: true,
        })
    }

    pub(crate) fn commit(
        mut self,
        shared: &ExecutorShared,
        generation: GenerationPin,
        id: u64,
    ) -> CompletionGuard {
        self.armed = false;

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
        if self.armed {
            release_active(self.shared);
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
        // SAFETY: executor shared state remains valid through Drop.
        let shared = unsafe { self.shared.get() };
        generation.get().remove_task(self.id);
        shared
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::EndAsyncTask(
                self.observation.completion(),
            ));
        // Release the generation before active: shutdown may reclaim the
        // entire executor immediately after the last active task departs.
        drop(generation);
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
