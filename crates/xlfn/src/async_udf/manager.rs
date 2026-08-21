use super::*;

pub(crate) const MAX_PENDING: usize = 4096;
pub(crate) const MAX_ASYNC_HANDLE_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(crate) struct AsyncStopped {
    _private: (),
}

impl AsyncStopped {
    fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(crate) const fn for_test() -> Self {
        Self { _private: () }
    }
}

pub(crate) struct AsyncManager {
    pub(crate) state: Mutex<ExecutorState>,
    /// Lock-free snapshot used by the async-UDF spawn hot path.
    ///
    /// `Some` while an executor is published for new spawns.
    /// `None` while stopped or closing.
    pub(crate) published_executor: ArcSwapOption<ExecutorShared>,
    pub(crate) state_changed: Condvar,
    pub(crate) generation_transition: Mutex<()>,
    pub(crate) current_generation: AtomicU64,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    pub(crate) after_generation_publish_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_spawn_handle_snapshot_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) before_generation_transition_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub(crate) enum ExecutorState {
    Stopped,
    Running(Executor),
    // `None` means one close caller owns the executor while it waits without
    // holding `state`. Tests can put the executor back after a timed-out wait.
    Closing(Option<Executor>),
}

impl AsyncManager {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(ExecutorState::Stopped),
            published_executor: ArcSwapOption::const_empty(),
            state_changed: Condvar::new(),
            generation_transition: Mutex::new(()),
            current_generation: AtomicU64::new(1),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(test)]
            after_generation_publish_hook: Mutex::new(None),
            #[cfg(test)]
            after_spawn_handle_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            before_generation_transition_hook: Mutex::new(None),
        }
    }

    pub(crate) fn start(&self, worker_count: usize) -> XllResult<()> {
        let _generation_transition = self.generation_transition.lock();
        let mut state = self.state.lock();
        if !matches!(&*state, ExecutorState::Stopped) {
            return match &*state {
                ExecutorState::Running(_) => Ok(()),
                ExecutorState::Closing(_) => Err(XllError::Closing),
                ExecutorState::Stopped => unreachable!("executor state was checked above"),
            };
        }
        let executor = Executor::start(worker_count, self.current_generation())?;
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            executor.set_ghost(ghost);
        }
        let published_executor = Arc::clone(&executor.shared);
        *state = ExecutorState::Running(executor);
        self.published_executor.store(Some(published_executor));
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::StartAsyncExecutor);
        }
        Ok(())
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(Arc::clone(&ghost));
        let mut state = self.state.lock();
        match &mut *state {
            ExecutorState::Running(executor) | ExecutorState::Closing(Some(executor)) => {
                executor.set_ghost(ghost);
            }
            ExecutorState::Stopped | ExecutorState::Closing(None) => {}
        }
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.current_generation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn snapshot_spawn_executor(&self) -> Result<Arc<ExecutorShared>, (XllError, bool)> {
        if let Some(shared) = self.published_executor.load_full() {
            return Ok(shared);
        }

        let state = self.state.lock();
        match &*state {
            ExecutorState::Running(executor) => {
                debug_assert!(
                    self.published_executor.load().is_some(),
                    "running executor must have a published spawn root"
                );
                Ok(Arc::clone(&executor.shared))
            }
            ExecutorState::Stopped | ExecutorState::Closing(_) => Err((XllError::Closing, false)),
        }
    }

    pub(crate) fn spawn<F>(
        &self,
        generation: u64,
        future: F,
        cancellation: CancellationSource,
    ) -> XllResult<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let published = self.published_executor.load();
        let target = published.as_ref();
        #[cfg(test)]
        if target.is_some() {
            let hook = self.after_spawn_handle_snapshot_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let result = match target {
            Some(shared) => shared.spawn(generation, future, cancellation),
            None => Err(SpawnRejection {
                error: XllError::Closing,
                future,
                cancellation,
                cancel: false,
            }),
        };
        match result {
            Ok(()) => Ok(()),
            Err(rejection) => {
                if rejection.cancel {
                    cancel_source_no_unwind(&rejection.cancellation);
                }
                // Rejected futures and their captured user values must be
                // dropped after releasing all manager/lifecycle synchronization;
                // Drop may legitimately re-enter runtime APIs.
                drop(rejection.future);
                Err(rejection.error)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn cancel_generation(&self, generation: u64) {
        let target = match &*self.state.lock() {
            ExecutorState::Running(executor) => Some((Arc::clone(&executor.shared), generation)),
            ExecutorState::Stopped | ExecutorState::Closing(_) => None,
        };
        let tasks = target
            .map(|(shared, generation)| shared.cancel_generation(generation))
            .unwrap_or_default();
        // Manager state released — safe to invoke arbitrary Waker::wake().
        cancel_tasks(tasks);
    }

    pub(crate) fn cancel_current_generation(&self) {
        let target = match &*self.state.lock() {
            ExecutorState::Running(executor) => {
                Some((Arc::clone(&executor.shared), self.current_generation()))
            }
            ExecutorState::Stopped | ExecutorState::Closing(_) => None,
        };
        let tasks = target
            .map(|(shared, generation)| shared.cancel_generation(generation))
            .unwrap_or_default();
        // Manager state released — safe to invoke arbitrary Waker::wake().
        cancel_tasks(tasks);
    }

    pub(crate) fn advance_generation(&self) -> bool {
        #[cfg(test)]
        {
            let hook = self.before_generation_transition_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let _generation_transition = self.generation_transition.lock();
        let target = {
            let state = self.state.lock();
            match &*state {
                ExecutorState::Stopped => {
                    let current = self.current_generation();
                    self.current_generation
                        .store(current.wrapping_add(1), Ordering::Release);
                    None
                }
                ExecutorState::Running(executor) => {
                    Some((Arc::clone(&executor.shared), self.current_generation()))
                }
                ExecutorState::Closing(_) => return false,
            }
        };
        let advanced = match target {
            None => true,
            Some((shared, current)) => {
                let next = current.wrapping_add(1);
                if !shared.advance_generation(next) {
                    false
                } else {
                    let state = self.state.lock();
                    match &*state {
                        // Each successful `Executor::start` allocates exactly one fresh
                        // `ExecutorShared`. Arc identity therefore uniquely identifies one
                        // executor incarnation, independent of calculation generation IDs.
                        ExecutorState::Running(executor)
                            if Arc::ptr_eq(&executor.shared, &shared) =>
                        {
                            self.current_generation
                                .compare_exchange(
                                    current,
                                    next,
                                    Ordering::AcqRel,
                                    Ordering::Acquire,
                                )
                                .is_ok()
                        }
                        ExecutorState::Stopped
                        | ExecutorState::Running(_)
                        | ExecutorState::Closing(_) => false,
                    }
                }
            }
        };
        #[cfg(test)]
        if advanced {
            let hook = self.after_generation_publish_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        advanced
    }

    #[cfg(test)]
    pub(crate) fn set_after_generation_publish_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.after_generation_publish_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(crate) fn set_after_spawn_handle_snapshot_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.after_spawn_handle_snapshot_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(crate) fn set_before_generation_transition_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.before_generation_transition_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(crate) fn set_after_generation_snapshot_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        let state = self.state.lock();
        if let ExecutorState::Running(executor) = &*state {
            *executor.shared.after_generation_snapshot_hook.lock() = hook;
        } else if hook.is_some() {
            panic!("async executor must be running when installing a test hook");
        }
    }

    #[cfg(test)]
    pub(crate) fn set_after_generation_admission_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        let state = self.state.lock();
        if let ExecutorState::Running(executor) = &*state {
            *executor.shared.after_generation_admission_hook.lock() = hook;
        } else if hook.is_some() {
            panic!("async executor must be running when installing a test hook");
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_task_schedule_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        let state = self.state.lock();
        if let ExecutorState::Running(executor) = &*state {
            *executor.shared.before_task_schedule_hook.lock() = hook;
        } else if hook.is_some() {
            panic!("async executor must be running when installing a test hook");
        }
    }

    pub(crate) fn close(&self) -> crate::shutdown::StopOutcome<AsyncStopped> {
        let Some(executor) = self.take_executor_for_close() else {
            return crate::shutdown::StopOutcome {
                certificate: AsyncStopped::new(),
                issues: Vec::new(),
            };
        };
        let tasks = executor.shared.request_close();
        // Manager state released — cancel/abort and run arbitrary task cleanup
        // without blocking re-entry into cancellation or generation APIs.
        cancel_tasks(tasks);

        // Excel owns the XLL module lifetime. Returning while a worker can
        // still execute this module is unsound, so shutdown deliberately has
        // no timeout: a non-cooperative poll keeps xlAutoRemove blocked.
        if !executor.wait_for_idle() && !executor.drain_after_worker_failure() {
            // No worker remains that can release the outstanding task guards.
            // Returning an AsyncStopped certificate would permit unsafe removal.
            std::process::abort();
        }
        let issues = executor.finish_close();
        self.finish_close();
        crate::shutdown::StopOutcome {
            certificate: AsyncStopped::new(),
            issues,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(*self.state.lock(), ExecutorState::Stopped)
    }

    #[cfg(test)]
    pub(crate) fn close_with_timeout(&self, timeout: Duration) -> XllResult<()> {
        let Some(executor) = self.take_executor_for_close() else {
            return Ok(());
        };
        let tasks = executor.shared.request_close();
        // Manager state released — cancel/abort without holding any locks.
        cancel_tasks(tasks);

        if !executor.wait_for_idle_timeout(timeout) {
            self.restore_closing_executor(executor);
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::ASYNC_TIME,
            });
        }
        let issues = executor.finish_close();
        self.finish_close();
        if issues.is_empty() {
            Ok(())
        } else {
            Err(XllError::Panic)
        }
    }

    pub(crate) fn take_executor_for_close(&self) -> Option<Executor> {
        let mut state = self.state.lock();
        loop {
            match &*state {
                ExecutorState::Stopped => return None,
                ExecutorState::Running(_) | ExecutorState::Closing(Some(_)) => {
                    self.published_executor.store(None);
                    let previous = std::mem::replace(&mut *state, ExecutorState::Closing(None));
                    self.state_changed.notify_all();
                    return match previous {
                        ExecutorState::Running(executor)
                        | ExecutorState::Closing(Some(executor)) => Some(executor),
                        ExecutorState::Stopped | ExecutorState::Closing(None) => {
                            unreachable!("close ownership was checked while holding state")
                        }
                    };
                }
                ExecutorState::Closing(None) => self.state_changed.wait(&mut state),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_for_closing(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        while !matches!(*state, ExecutorState::Closing(_)) {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.state_changed.wait_for(&mut state, deadline - now);
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn restore_closing_executor(&self, executor: Executor) {
        let mut state = self.state.lock();
        debug_assert!(matches!(*state, ExecutorState::Closing(None)));
        debug_assert!(
            self.published_executor.load().is_none(),
            "closing executor must not be published for spawning"
        );
        *state = ExecutorState::Closing(Some(executor));
        self.state_changed.notify_all();
    }

    pub(crate) fn finish_close(&self) {
        let mut state = self.state.lock();
        debug_assert!(matches!(*state, ExecutorState::Closing(None)));
        debug_assert!(
            self.published_executor.load().is_none(),
            "closed executor must not be published for spawning"
        );
        *state = ExecutorState::Stopped;
        self.state_changed.notify_all();
    }
}
