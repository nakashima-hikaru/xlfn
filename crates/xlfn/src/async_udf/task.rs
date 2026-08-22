use super::executor::ExecutorShared;
use super::generation::GenerationState;
use super::manager::MAX_PENDING;
use super::worker::release_active;
use crate::cancellation::CancellationSource;
use crate::error::XllError;
use futures_util::future::AbortHandle;
#[cfg(any(test, feature = "unstable"))]
use parking_lot::Mutex;
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
            #[cfg(any(test, feature = "unstable"))]
            completion: Mutex::new(crate::shutdown_refinement::Completion::Failed),
            #[cfg(any(test, feature = "unstable"))]
            ghost: None,
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
    #[cfg(any(test, feature = "unstable"))]
    pub(crate) completion: Mutex<crate::shutdown_refinement::Completion>,
    #[cfg(any(test, feature = "unstable"))]
    pub(crate) ghost: Option<crate::shutdown_refinement::GhostHandle>,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.generation.remove_task(self.id);
        #[cfg(any(test, feature = "unstable"))]
        if let Some(ghost) = self.ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::EndAsyncTask(
                *self.completion.lock(),
            ));
        }
        release_active(&self.shared);
    }
}
