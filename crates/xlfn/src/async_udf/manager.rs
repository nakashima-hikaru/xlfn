use super::executor::{Executor, ExecutorShared};
use super::task::SpawnRejection;
use super::worker::{cancel_source_no_unwind, cancel_tasks};
use crate::cancellation::CancellationSource;
#[cfg(test)]
use crate::diagnostics::id::DiagnosticId;
use crate::{XllError, XllResult};
use futures_util::Future;
use parking_lot::{Condvar, Mutex};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use xlfn_kernel::drain_gate::{DrainGate, DrainPermit};
#[cfg(test)]
use std::time::{Duration, Instant};

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
}

pub(crate) struct AsyncManager {
    pub(crate) state: Mutex<ExecutorState>,
    /// Non-owning lock-free publication for the async-UDF spawn hot path.
    pub(crate) published_executor: AtomicPtr<ExecutorShared>,
    /// Readers admitted through this gate may dereference the publication.
    /// Close withdraws publication, drains readers, then reclaims the Box only
    /// after active tasks and workers have also quiesced.
    pub(crate) spawn_admission: DrainGate,
    pub(crate) state_changed: Condvar,
    pub(crate) generation_transition: Mutex<()>,
    pub(crate) current_generation: AtomicU64,
    pub(crate) observer: crate::shutdown_trace::ObservationSink,
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

pub(crate) struct ExecutorRead<'manager> {
    shared: NonNull<ExecutorShared>,
    _permit: DrainPermit<'manager>,
}

impl Deref for ExecutorRead<'_> {
    type Target = ExecutorShared;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the spawn-admission permit prevents executor reclamation.
        unsafe { self.shared.as_ref() }
    }
}

impl AsyncManager {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(ExecutorState::Stopped),
            published_executor: AtomicPtr::new(std::ptr::null_mut()),
            spawn_admission: DrainGate::new_sealed(),
            state_changed: Condvar::new(),
            generation_transition: Mutex::new(()),
            current_generation: AtomicU64::new(1),
            observer: crate::shutdown_trace::ObservationSink::new(),
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
        if let Some(trace) = self.observer.trace_handle() {
            executor.set_trace_sink(trace);
        }
        let published_executor = NonNull::from(executor.shared.as_ref());
        *state = ExecutorState::Running(executor);
        self.published_executor
            .store(published_executor.as_ptr(), Ordering::Release);
        self.spawn_admission
            .reopen()
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        drop(state);
        self.observer
            .record(crate::shutdown_trace::ShutdownEvent::StartAsyncExecutor);
        Ok(())
    }

    #[allow(
        dead_code,
        reason = "trace wiring is used only when the runtime observer is enabled"
    )]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.observer.set_trace_sink(Arc::clone(&trace));
        let mut state = self.state.lock();
        match &mut *state {
            ExecutorState::Running(executor) | ExecutorState::Closing(Some(executor)) => {
                executor.set_trace_sink(trace);
            }
            ExecutorState::Stopped | ExecutorState::Closing(None) => {}
        }
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.current_generation.load(Ordering::Acquire)
    }

    #[cfg(feature = "bench-internals")]
    pub(crate) fn wait_idle(&self) -> bool {
        let Some(shared) = self.published_executor() else {
            return true;
        };
        let mut guard = shared.wait_lock.lock();
        while shared.active.load(Ordering::Acquire) != 0 {
            if shared.fatal_worker_failure.load(Ordering::Acquire)
                && shared.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            shared.idle.wait(&mut guard);
        }
        true
    }

    fn published_executor(&self) -> Option<ExecutorRead<'_>> {
        let permit = self.spawn_admission.try_enter().ok()?;
        let shared = NonNull::new(self.published_executor.load(Ordering::Acquire))?;
        Some(ExecutorRead {
            shared,
            _permit: permit,
        })
    }

    #[cfg(test)]
    pub(crate) fn snapshot_spawn_executor(&self) -> Result<ExecutorRead<'_>, (XllError, bool)> {
        self.published_executor()
            .ok_or((XllError::Closing, false))
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
        let target = self.published_executor();
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
        let tasks = self
            .published_executor()
            .map(|executor| executor.cancel_generation(generation))
            .unwrap_or_default();
        // Manager state released — safe to invoke arbitrary Waker::wake().
        cancel_tasks(tasks);
    }

    pub(crate) fn cancel_current_generation(&self) {
        let tasks = self
            .published_executor()
            .map(|executor| executor.cancel_generation(self.current_generation()))
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
        let current = self.current_generation();
        let Some(executor) = self.published_executor() else {
            let state = self.state.lock();
            return match &*state {
                ExecutorState::Stopped => {
                    self.current_generation
                        .store(current.wrapping_add(1), Ordering::Release);
                    true
                }
                ExecutorState::Running(_) | ExecutorState::Closing(_) => false,
            };
        };
        let executor_pointer = NonNull::from(&*executor);
        let next = current.wrapping_add(1);
        let transitioned = executor.advance_generation(next);
        drop(executor);
        let advanced = if !transitioned {
            false
        } else {
            let state = self.state.lock();
            match &*state {
                ExecutorState::Running(current_executor)
                    if NonNull::from(current_executor.shared.as_ref()) == executor_pointer =>
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
        self.spawn_admission.wait_until_idle();

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

    pub(crate) fn is_running(&self) -> bool {
        matches!(*self.state.lock(), ExecutorState::Running(_))
    }

    #[allow(
        dead_code,
        reason = "the observer samples executor state only in refinement builds"
    )]
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
        self.spawn_admission.wait_until_idle();

        if !executor.wait_for_idle_timeout(timeout) {
            self.restore_closing_executor(executor);
            return Err(XllError::Internal {
                diagnostic_id: DiagnosticId::ASYNC_TIME,
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
                    self.spawn_admission.seal();
                    self.published_executor
                        .store(std::ptr::null_mut(), Ordering::Release);
                    let previous = std::mem::replace(&mut *state, ExecutorState::Closing(None));
                    self.state_changed.notify_all();
                    let executor = match previous {
                        ExecutorState::Running(executor)
                        | ExecutorState::Closing(Some(executor)) => Some(executor),
                        ExecutorState::Stopped | ExecutorState::Closing(None) => {
                            unreachable!("close ownership was checked while holding state")
                        }
                    };
                    return executor;
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
            self.published_executor.load(Ordering::Acquire).is_null(),
            "closing executor must not be published for spawning"
        );
        *state = ExecutorState::Closing(Some(executor));
        self.state_changed.notify_all();
    }

    pub(crate) fn finish_close(&self) {
        let mut state = self.state.lock();
        debug_assert!(matches!(*state, ExecutorState::Closing(None)));
        debug_assert!(
            self.published_executor.load(Ordering::Acquire).is_null(),
            "closed executor must not be published for spawning"
        );
        *state = ExecutorState::Stopped;
        self.state_changed.notify_all();
    }
}
