use super::executor::ExecutorShared;
use super::generation::GenerationState;
use super::manager::MAX_PENDING;
use super::worker::release_active;
use crate::cancellation::CancellationSource;
use crate::error::XllError;
use futures_util::future::AbortHandle;
use std::sync::Arc;
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
        shared: &Arc<ExecutorShared>,
        generation: triomphe::Arc<GenerationState>,
        id: u64,
    ) -> CompletionGuard {
        self.armed = false;

        CompletionGuard {
            shared: Arc::clone(shared),
            generation,
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
    pub(crate) shared: Arc<ExecutorShared>,
    pub(crate) generation: triomphe::Arc<GenerationState>,
    pub(crate) id: u64,
    pub(crate) observation: CompletionObservation,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.generation.remove_task(self.id);
        self.shared
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::EndAsyncTask(
                self.observation.completion(),
            ));
        release_active(&self.shared);
    }
}

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
