use super::*;

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

pub(crate) struct ActiveReservation {
    pub(crate) inner: Arc<ExecutorInner>,
    pub(crate) armed: bool,
}

impl ActiveReservation {
    pub(crate) fn try_acquire(inner: Arc<ExecutorInner>) -> Option<Self> {
        inner
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_PENDING).then_some(active + 1)
            })
            .ok()?;

        Some(Self { inner, armed: true })
    }

    pub(crate) fn commit(mut self, generation: Arc<GenerationState>, id: u64) -> CompletionGuard {
        self.armed = false;

        CompletionGuard {
            inner: Arc::clone(&self.inner),
            generation,
            id,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            completion: Mutex::new(crate::shutdown_refinement::Completion::Failed),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: None,
        }
    }
}

impl Drop for ActiveReservation {
    fn drop(&mut self) {
        if self.armed {
            release_active(&self.inner);
        }
    }
}

pub(crate) struct CompletionGuard {
    pub(crate) inner: Arc<ExecutorInner>,
    pub(crate) generation: Arc<GenerationState>,
    pub(crate) id: u64,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) completion: Mutex<crate::shutdown_refinement::Completion>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Option<crate::shutdown_refinement::GhostHandle>,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.generation.remove_task(self.id);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::EndAsyncTask(
                *self.completion.lock(),
            ));
        }
        release_active(&self.inner);
    }
}
