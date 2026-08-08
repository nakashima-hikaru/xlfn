#![allow(unsafe_code, reason = "Low-level FFI interaction for async UDF tasks")]
#![cfg(feature = "async")]

use crate::cancellation::CancellationSource;
use crate::return_value::AsyncReturnPointer;
use crate::{
    CallId, CallMetadata, CallOutcome, CancellationGuarantee, CancellationToken, IntoExcelValue,
    Runtime, UdfResultKind, XllError, XllResult,
};
use arc_swap::ArcSwap;
use async_channel::{Receiver, Sender};
use async_task::Runnable;
use futures_util::FutureExt;
use futures_util::future::{AbortHandle, Abortable};
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;
use xlfn_sys::{
    XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA, XLTYPE_BOOL,
};

const MAX_PENDING: usize = 4096;
const MAX_ASYNC_HANDLE_BYTES: usize = 1024 * 1024;
pub(crate) struct AsyncManager {
    state: Mutex<ExecutorState>,
    state_changed: Condvar,
    generation_transition: Mutex<()>,
    current_generation: AtomicU64,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    after_generation_publish_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_spawn_handle_snapshot_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    before_generation_transition_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

enum ExecutorState {
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
        *state = ExecutorState::Running(executor);
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

    pub(crate) fn spawn<F>(
        &self,
        generation: u64,
        future: F,
        cancellation: CancellationSource,
    ) -> XllResult<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let target: Result<ExecutorHandle, (XllError, bool)> = {
            let state = self.state.lock();
            match &*state {
                ExecutorState::Running(executor) => Ok(executor.handle.clone()),
                ExecutorState::Stopped | ExecutorState::Closing(_) => {
                    Err((XllError::Closing, false))
                }
            }
        };
        #[cfg(test)]
        if target.is_ok() {
            let hook = self.after_spawn_handle_snapshot_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let result = match target {
            Ok(handle) => handle.spawn(generation, future, cancellation),
            Err((error, cancel)) => Err(SpawnRejection {
                error,
                future,
                cancellation,
                cancel,
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
    fn cancel_generation(&self, generation: u64) {
        let target = match &*self.state.lock() {
            ExecutorState::Running(executor) => Some((executor.handle.clone(), generation)),
            ExecutorState::Stopped | ExecutorState::Closing(_) => None,
        };
        let tasks = target
            .map(|(handle, generation)| handle.cancel_generation(generation))
            .unwrap_or_default();
        // Manager state released — safe to invoke arbitrary Waker::wake().
        cancel_tasks(tasks);
    }

    pub(crate) fn cancel_current_generation(&self) {
        let target = match &*self.state.lock() {
            ExecutorState::Running(executor) => {
                Some((executor.handle.clone(), self.current_generation()))
            }
            ExecutorState::Stopped | ExecutorState::Closing(_) => None,
        };
        let tasks = target
            .map(|(handle, generation)| handle.cancel_generation(generation))
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
                    Some((executor.handle.clone(), self.current_generation()))
                }
                ExecutorState::Closing(_) => return false,
            }
        };
        let advanced = match target {
            None => true,
            Some((handle, current)) => {
                let next = current.wrapping_add(1);
                if !handle.advance_generation(next) {
                    false
                } else {
                    let state = self.state.lock();
                    match &*state {
                        ExecutorState::Running(executor)
                            if Arc::ptr_eq(&executor.handle.inner, &handle.inner) =>
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
    fn set_after_spawn_handle_snapshot_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.after_spawn_handle_snapshot_hook.lock() = hook;
    }

    #[cfg(test)]
    fn set_before_generation_transition_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.before_generation_transition_hook.lock() = hook;
    }

    #[cfg(test)]
    fn set_after_generation_snapshot_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        let state = self.state.lock();
        if let ExecutorState::Running(executor) = &*state {
            *executor.handle.inner.after_generation_snapshot_hook.lock() = hook;
        } else if hook.is_some() {
            panic!("async executor must be running when installing a test hook");
        }
    }

    #[cfg(test)]
    fn set_after_generation_admission_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        let state = self.state.lock();
        if let ExecutorState::Running(executor) = &*state {
            *executor.handle.inner.after_generation_admission_hook.lock() = hook;
        } else if hook.is_some() {
            panic!("async executor must be running when installing a test hook");
        }
    }

    #[cfg(test)]
    fn set_before_task_schedule_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        let state = self.state.lock();
        if let ExecutorState::Running(executor) = &*state {
            *executor.handle.inner.before_task_schedule_hook.lock() = hook;
        } else if hook.is_some() {
            panic!("async executor must be running when installing a test hook");
        }
    }

    pub(crate) fn close(&self) -> crate::shutdown::StopOutcome<crate::shutdown::AsyncStopped> {
        let Some(executor) = self.take_executor_for_close() else {
            return crate::shutdown::StopOutcome {
                certificate: crate::shutdown::AsyncStopped::new(),
                issues: Vec::new(),
            };
        };
        let tasks = executor.handle.request_close();
        // Manager state released — cancel/abort and run arbitrary task cleanup
        // without blocking re-entry into cancellation or generation APIs.
        cancel_tasks(tasks);

        // Excel owns the XLL module lifetime. Returning while a worker can
        // still execute this module is unsound, so shutdown deliberately has
        // no timeout: a non-cooperative poll keeps xlAutoClose blocked.
        if !executor.wait_for_idle() && !executor.drain_after_worker_failure() {
            // No worker remains that can release the outstanding task guards.
            // Returning an AsyncStopped certificate would permit unsafe unload.
            std::process::abort();
        }
        let issues = executor.finish_close();
        self.finish_close();
        crate::shutdown::StopOutcome {
            certificate: crate::shutdown::AsyncStopped::new(),
            issues,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(*self.state.lock(), ExecutorState::Stopped)
    }

    #[cfg(test)]
    fn close_with_timeout(&self, timeout: Duration) -> XllResult<()> {
        let Some(executor) = self.take_executor_for_close() else {
            return Ok(());
        };
        let tasks = executor.handle.request_close();
        // Manager state released — cancel/abort without holding any locks.
        cancel_tasks(tasks);

        if !executor.wait_for_idle_timeout(timeout) {
            self.restore_closing_executor(executor);
            return Err(XllError::Internal {
                diagnostic_id: 0x4153_594e_5449_4d45,
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

    fn take_executor_for_close(&self) -> Option<Executor> {
        let mut state = self.state.lock();
        loop {
            match &*state {
                ExecutorState::Stopped => return None,
                ExecutorState::Running(_) | ExecutorState::Closing(Some(_)) => {
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
    fn wait_for_closing(&self, timeout: Duration) -> bool {
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
    fn restore_closing_executor(&self, executor: Executor) {
        let mut state = self.state.lock();
        debug_assert!(matches!(*state, ExecutorState::Closing(None)));
        *state = ExecutorState::Closing(Some(executor));
        self.state_changed.notify_all();
    }

    fn finish_close(&self) {
        let mut state = self.state.lock();
        debug_assert!(matches!(*state, ExecutorState::Closing(None)));
        *state = ExecutorState::Stopped;
        self.state_changed.notify_all();
    }
}

struct Executor {
    handle: ExecutorHandle,
    receiver: Receiver<Runnable>,
    workers: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
struct ExecutorHandle {
    inner: Arc<ExecutorInner>,
    sender: Sender<Runnable>,
}

/// Inner state of `Executor`.
///
/// Invariants:
/// I1. `current`'s `GenerationState` always exists in `control.generations` until `ControlPhase::Closing`.
/// I2. When `control.phase == ControlPhase::Running`, `current.admission` is the admission authority for the current generation.
/// I3. When `control.phase == ControlPhase::Advancing { from, to }`, `current.id == from` and `current.admission` is closed.
/// I4. After `control.phase == ControlPhase::Closing`, no new `GenerationState` is ever published to `current`.
/// I5. A `GenerationState` may be removed from `control.generations` only when `generation != next` and `task_count == 0`.
struct ExecutorInner {
    next_id: AtomicU64,
    active: AtomicUsize,
    live_workers: AtomicUsize,
    fatal_worker_failure: AtomicBool,
    /// Monotonic fast-path mirror of `ExecutorControl::phase == ControlPhase::Closing`.
    ///
    /// Lifecycle transitions are authoritative under `control`;
    /// spawn reads only this atomic.
    closing: AtomicBool,
    current: ArcSwap<GenerationState>,
    /// Cold lifecycle state. Never acquired by spawn/completion.
    control: Mutex<ExecutorControl>,
    wait_lock: Mutex<()>,
    idle: Condvar,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    before_task_schedule_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_generation_snapshot_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_generation_admission_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlPhase {
    Running,
    Advancing { from: u64, to: u64 },
    Closing,
}

const TASK_SHARDS: usize = 32;

fn task_shard(id: u64) -> usize {
    (id as usize) & (TASK_SHARDS - 1)
}

struct TaskShard {
    tasks: Mutex<HashMap<u64, TaskControl>>,
}

const ADMISSION_CLOSED: usize = 1usize << (usize::BITS - 1);
const ADMISSION_COUNT_MASK: usize = ADMISSION_CLOSED - 1;

struct GenerationAdmission {
    state: AtomicUsize,
    wait_lock: Mutex<()>,
    idle: Condvar,
}

impl GenerationAdmission {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    fn try_enter(&self) -> Option<AdmissionPermit<'_>> {
        loop {
            let state = self.state.load(Ordering::Acquire);

            if state & ADMISSION_CLOSED != 0 {
                return None;
            }

            let active = state & ADMISSION_COUNT_MASK;
            if active == ADMISSION_COUNT_MASK {
                std::process::abort();
            }

            let next = state + 1;

            if self
                .state
                .compare_exchange_weak(state, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(AdmissionPermit { admission: self });
            }
        }
    }

    fn close(&self) {
        self.state.fetch_or(ADMISSION_CLOSED, Ordering::AcqRel);
    }

    fn wait_for_idle(&self) {
        let mut guard = self.wait_lock.lock();
        while self.state.load(Ordering::Acquire) & ADMISSION_COUNT_MASK != 0 {
            self.idle.wait(&mut guard);
        }
    }
}

struct AdmissionPermit<'a> {
    admission: &'a GenerationAdmission,
}

impl Drop for AdmissionPermit<'_> {
    fn drop(&mut self) {
        let previous = self.admission.state.fetch_sub(1, Ordering::AcqRel);

        debug_assert_ne!(
            previous & ADMISSION_COUNT_MASK,
            0,
            "generation admission count must remain balanced"
        );

        if previous & ADMISSION_CLOSED != 0 && previous & ADMISSION_COUNT_MASK == 1 {
            let _guard = self.admission.wait_lock.lock();
            self.admission.idle.notify_all();
        }
    }
}

struct GenerationState {
    id: u64,
    admission: GenerationAdmission,
    task_count: AtomicUsize,
    shards: Box<[TaskShard]>,
}

impl GenerationState {
    fn new(id: u64) -> Self {
        let shards = (0..TASK_SHARDS)
            .map(|_| TaskShard {
                tasks: Mutex::new(HashMap::new()),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            id,
            admission: GenerationAdmission::new(),
            task_count: AtomicUsize::new(0),
            shards,
        }
    }

    fn remove_task(&self, id: u64) -> bool {
        let index = task_shard(id);
        let mut tasks = self.shards[index].tasks.lock();
        if tasks.remove(&id).is_some() {
            self.task_count.fetch_sub(1, Ordering::AcqRel);
            true
        } else {
            false
        }
    }

    fn drain_tasks(&self) -> Vec<TaskControl> {
        let mut result = Vec::new();
        for shard in self.shards.iter() {
            let mut tasks = shard.tasks.lock();
            let count = tasks.len();
            let drained = tasks.drain().map(|(_, task)| task).collect::<Vec<_>>();
            result.extend(drained);
            if count != 0 {
                self.task_count.fetch_sub(count, Ordering::AcqRel);
            }
        }
        result
    }
}

struct ExecutorControl {
    phase: ControlPhase,
    generations: HashMap<u64, Arc<GenerationState>>,
}

struct TaskControl {
    abort: AbortHandle,
    cancellation: CancellationSource,
}

struct SpawnRejection<F> {
    error: XllError,
    future: F,
    cancellation: CancellationSource,
    cancel: bool,
}

struct ActiveReservation {
    inner: Arc<ExecutorInner>,
    armed: bool,
}

impl ActiveReservation {
    fn try_acquire(inner: Arc<ExecutorInner>) -> Option<Self> {
        inner
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_PENDING).then_some(active + 1)
            })
            .ok()?;

        Some(Self { inner, armed: true })
    }

    fn commit(mut self, generation: Arc<GenerationState>, id: u64) -> CompletionGuard {
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

impl Executor {
    fn start(worker_count: usize, generation: u64) -> XllResult<Self> {
        let worker_count = worker_count.clamp(1, 32);
        let (sender, receiver) = async_channel::unbounded::<Runnable>();
        let initial_generation = Arc::new(GenerationState::new(generation));
        let inner = Arc::new(ExecutorInner {
            next_id: AtomicU64::new(1),
            active: AtomicUsize::new(0),
            live_workers: AtomicUsize::new(0),
            fatal_worker_failure: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            current: ArcSwap::from(Arc::clone(&initial_generation)),
            control: Mutex::new(ExecutorControl {
                phase: ControlPhase::Running,
                generations: HashMap::from([(generation, initial_generation)]),
            }),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(test)]
            before_task_schedule_hook: Mutex::new(None),
            #[cfg(test)]
            after_generation_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            after_generation_admission_hook: Mutex::new(None),
        });
        let mut workers = scopeguard::guard(
            Vec::<JoinHandle<()>>::with_capacity(worker_count),
            |mut workers| {
                sender.close();
                loop {
                    let Some(worker) = workers.pop() else {
                        break;
                    };
                    drop(worker.join());
                }
            },
        );
        for index in 0..worker_count {
            let receiver = receiver.clone();
            let worker_inner = Arc::clone(&inner);
            inner.live_workers.fetch_add(1, Ordering::Release);
            let worker = thread::Builder::new()
                .name(format!("xlfn-async-{index}"))
                .spawn(move || {
                    let _exit = WorkerExitGuard {
                        inner: worker_inner,
                    };
                    run_executor(receiver);
                });
            let worker = match worker {
                Ok(worker) => worker,
                Err(_) => {
                    inner.live_workers.fetch_sub(1, Ordering::AcqRel);
                    return Err(XllError::Internal {
                        diagnostic_id: 0x4153_594e_4353_504e,
                    });
                }
            };
            workers.push(worker);
        }
        let workers = scopeguard::ScopeGuard::into_inner(workers);
        let handle = ExecutorHandle {
            inner: Arc::clone(&inner),
            sender: sender.clone(),
        };
        Ok(Self {
            handle,
            receiver,
            workers,
        })
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.handle.inner.ghost.lock() = Some(ghost);
    }
}

impl ExecutorHandle {
    fn spawn<F>(
        &self,
        generation: u64,
        future: F,
        cancellation: CancellationSource,
    ) -> Result<(), SpawnRejection<F>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(SpawnRejection {
                error: XllError::Closing,
                future,
                cancellation,
                cancel: true,
            });
        }

        let current = self.inner.current.load_full();

        #[cfg(test)]
        {
            let hook = self.inner.after_generation_snapshot_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }

        if current.id != generation {
            return Err(SpawnRejection {
                error: cancelled_calculation_error(),
                future,
                cancellation,
                cancel: true,
            });
        }

        let Some(admission) = current.admission.try_enter() else {
            let error = if self.inner.closing.load(Ordering::Acquire) {
                XllError::Closing
            } else {
                cancelled_calculation_error()
            };

            return Err(SpawnRejection {
                error,
                future,
                cancellation,
                cancel: true,
            });
        };

        #[cfg(test)]
        {
            let hook = self.inner.after_generation_admission_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }

        let Some(reservation) = ActiveReservation::try_acquire(Arc::clone(&self.inner)) else {
            drop(admission);
            return Err(SpawnRejection {
                error: XllError::Overloaded,
                future,
                cancellation,
                cancel: false,
            });
        };

        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (abort, registration) = AbortHandle::new_pair();

        let index = task_shard(id);
        {
            let mut tasks = current.shards[index].tasks.lock();
            let previous = tasks.insert(
                id,
                TaskControl {
                    abort,
                    cancellation,
                },
            );
            debug_assert!(previous.is_none(), "task ID must be unique per generation");
            current.task_count.fetch_add(1, Ordering::AcqRel);
        }

        #[allow(
            unused_mut,
            reason = "completion.ghost is mutated only when feature-gated ghost recording is active"
        )]
        let mut completion = reservation.commit(Arc::clone(&current), id);

        drop(admission);

        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.inner.ghost.lock().as_ref().cloned() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::StartAsyncTask);
            completion.ghost = Some(ghost);
        }

        let wrapped = async move {
            let _completion = completion;
            #[cfg(any(test, feature = "shutdown-refinement"))]
            let result = Abortable::new(future, registration).await;
            #[cfg(any(test, feature = "shutdown-refinement"))]
            {
                *_completion.completion.lock() = if result.is_ok() {
                    crate::shutdown_refinement::Completion::Completed
                } else {
                    crate::shutdown_refinement::Completion::Canceled
                };
            }
            #[cfg(not(any(test, feature = "shutdown-refinement")))]
            let _ = Abortable::new(future, registration).await;
        };
        #[cfg(test)]
        {
            let hook = self.inner.before_task_schedule_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let sender = self.sender.clone();
        let schedule = move |runnable| {
            let _ = sender.try_send(runnable);
        };
        let (runnable, task) = async_task::spawn(wrapped, schedule);
        task.detach();
        runnable.schedule();
        Ok(())
    }

    fn cancel_generation(&self, generation: u64) -> Vec<TaskControl> {
        let generation_arc = {
            let control = self.inner.control.lock();
            let Some(state) = control.generations.get(&generation) else {
                return Vec::new();
            };
            debug_assert_eq!(state.id, generation);
            state.admission.close();
            Arc::clone(state)
        };
        generation_arc.admission.wait_for_idle();
        generation_arc.drain_tasks()
    }

    fn advance_generation(&self, next: u64) -> bool {
        let old = {
            let mut control = self.inner.control.lock();
            match control.phase {
                ControlPhase::Running => {}
                ControlPhase::Closing => return false,
                ControlPhase::Advancing { .. } => {
                    debug_assert!(false, "concurrent executor generation transition");
                    return false;
                }
            }

            let old = self.inner.current.load_full();
            old.admission.close();
            control.phase = ControlPhase::Advancing {
                from: old.id,
                to: next,
            };
            old
        };

        old.admission.wait_for_idle();

        let mut control = self.inner.control.lock();
        match control.phase {
            ControlPhase::Closing => return false,
            ControlPhase::Advancing { from, to } if from == old.id && to == next => {}
            ControlPhase::Running | ControlPhase::Advancing { .. } => {
                debug_assert!(false, "executor generation transition state diverged");
                return false;
            }
        }

        let next_generation = control
            .generations
            .entry(next)
            .or_insert_with(|| Arc::new(GenerationState::new(next)))
            .clone();

        self.inner.current.store(Arc::clone(&next_generation));

        control.generations.retain(|generation, state| {
            *generation == next || state.task_count.load(Ordering::Acquire) != 0
        });

        control.phase = ControlPhase::Running;
        true
    }

    fn request_close(&self) -> Vec<TaskControl> {
        let generations = {
            let mut control = self.inner.control.lock();

            if matches!(control.phase, ControlPhase::Closing) {
                return Vec::new();
            }

            self.inner.closing.store(true, Ordering::Release);
            control.phase = ControlPhase::Closing;

            let generations = control.generations.values().cloned().collect::<Vec<_>>();
            for generation in &generations {
                generation.admission.close();
            }
            generations
        };

        for generation in &generations {
            generation.admission.wait_for_idle();
        }

        let mut tasks = Vec::new();
        for generation in generations {
            tasks.extend(generation.drain_tasks());
        }
        tasks
    }
}

impl Executor {
    fn wait_for_idle(&self) -> bool {
        let mut guard = self.handle.inner.wait_lock.lock();
        while self.handle.inner.active.load(Ordering::Acquire) != 0 {
            if self
                .handle
                .inner
                .fatal_worker_failure
                .load(Ordering::Acquire)
                && self.handle.inner.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            self.handle.inner.idle.wait(&mut guard);
        }
        true
    }

    #[cfg(test)]
    fn wait_for_idle_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.handle.inner.wait_lock.lock();
        while self.handle.inner.active.load(Ordering::Acquire) != 0 {
            if self
                .handle
                .inner
                .fatal_worker_failure
                .load(Ordering::Acquire)
                && self.handle.inner.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.handle.inner.idle.wait_for(&mut guard, deadline - now);
        }
        true
    }

    fn drain_after_worker_failure(&self) -> bool {
        self.handle.sender.close();
        while let Ok(runnable) = self.receiver.try_recv() {
            drop(runnable);
        }
        self.handle.inner.active.load(Ordering::Acquire) == 0
    }

    fn finish_close(mut self) -> Vec<crate::shutdown::CleanupIssue> {
        self.handle.sender.close();
        let mut issues = Vec::new();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                issues.push(crate::shutdown::CleanupIssue {
                    component: "async worker",
                    kind: crate::CleanupIssueKind::WorkerPanickedAfterJoin,
                    error: XllError::Panic,
                });
            }
        }
        issues
    }
}

struct WorkerExitGuard {
    inner: Arc<ExecutorInner>,
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.inner
                .fatal_worker_failure
                .store(true, Ordering::Release);
        }
        self.inner.live_workers.fetch_sub(1, Ordering::AcqRel);
        let _guard = self.inner.wait_lock.lock();
        self.inner.idle.notify_all();
    }
}

fn release_active(inner: &ExecutorInner) {
    if inner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
        let _guard = inner.wait_lock.lock();
        inner.idle.notify_all();
    }
}

struct CompletionGuard {
    inner: Arc<ExecutorInner>,
    generation: Arc<GenerationState>,
    id: u64,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    completion: Mutex<crate::shutdown_refinement::Completion>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Option<crate::shutdown_refinement::GhostHandle>,
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

fn cancelled_calculation_error() -> XllError {
    XllError::ExcelValue(crate::ExcelError::NotAvailable)
}

/// Cancels and aborts a batch of tasks outside of any lock.
fn cancel_tasks(tasks: Vec<TaskControl>) {
    for TaskControl {
        abort,
        cancellation,
    } in tasks
    {
        // CancellationToken is public and may have been polled by an arbitrary
        // executor. Its Waker::wake implementation is user-controlled and may
        // panic; one such panic must not prevent this or later tasks from being
        // aborted, especially while AsyncManager owns a Closing executor.
        cancel_source_no_unwind(&cancellation);
        let _ = catch_unwind(AssertUnwindSafe(|| abort.abort()));
    }
}

fn cancel_source_no_unwind(cancellation: &CancellationSource) {
    let _ = catch_unwind(AssertUnwindSafe(|| cancellation.cancel()));
}

fn run_executor(receiver: Receiver<Runnable>) {
    loop {
        let Ok(runnable) = receiver.recv_blocking() else {
            break;
        };
        // Panics from user futures are contained by the UDF wrapper. Anything
        // that reaches this executor boundary is an infrastructure failure and
        // must terminate the worker so close can report it and explicitly
        // dispose any work stranded after the last worker exits.
        runnable.run();
    }
}

struct OwnedAsyncHandle {
    udf_id: &'static str,
    raw: XLOPER12,
    bytes: Option<Box<[u8]>>,
    completed: bool,
    fallback_error: Option<XllError>,
}

// SAFETY: construction owns any pointed-to bytes; an opaque zero-length handle
// is only copied back to Excel and is never dereferenced by Rust.
unsafe impl Send for OwnedAsyncHandle {}

impl OwnedAsyncHandle {
    unsafe fn from_raw(udf_id: &'static str, raw: *mut XLOPER12) -> XllResult<Self> {
        // SAFETY: the caller guarantees a live Excel async-handle argument.
        let value = unsafe { raw.as_ref() }.ok_or_else(|| {
            XllError::input(
                "async_handle",
                crate::InputError::Malformed("null async handle"),
            )
        })?;
        if value.base_type() != XLTYPE_BIG_DATA {
            return Err(XllError::input(
                "async_handle",
                crate::InputError::Malformed("expected xltypeBigData"),
            ));
        }
        // SAFETY: XLTYPE_BIG_DATA selects the big_data union field.
        let big_data = unsafe { value.value.big_data };
        let byte_count = usize::try_from(big_data.byte_count).map_err(|_| {
            XllError::input(
                "async_handle",
                crate::InputError::Malformed("negative async handle size"),
            )
        })?;
        if byte_count > MAX_ASYNC_HANDLE_BYTES {
            return Err(XllError::input(
                "async_handle",
                crate::InputError::Malformed("async handle is too large"),
            ));
        }
        let mut bytes = if byte_count == 0 {
            None
        } else {
            // SAFETY: a positive byte count selects the data pointer representation.
            let data = unsafe { big_data.handle.data };
            if data.is_null() {
                return Err(XllError::input(
                    "async_handle",
                    crate::InputError::Malformed("null async handle data"),
                ));
            }
            // SAFETY: Excel promises byte_count readable bytes for this call.
            Some(
                unsafe { std::slice::from_raw_parts(data, byte_count) }
                    .to_vec()
                    .into_boxed_slice(),
            )
        };
        let handle = bytes
            .as_mut()
            .map_or(big_data.handle, |bytes| XLOPER12BigDataHandle {
                data: bytes.as_mut_ptr(),
            });
        Ok(Self {
            udf_id,
            raw: XLOPER12 {
                value: XLOPER12Value {
                    big_data: XLOPER12BigData {
                        handle,
                        byte_count: big_data.byte_count,
                    },
                },
                xltype: XLTYPE_BIG_DATA,
            },
            bytes,
            completed: false,
            fallback_error: None,
        })
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        let _ = &self.bytes;
        NonNull::from(&mut self.raw)
    }

    fn set_error(&mut self, error: XllError) {
        self.fallback_error = Some(error);
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for OwnedAsyncHandle {
    fn drop(&mut self) {
        if !self.completed {
            self.completed = true;
            let error = self
                .fallback_error
                .take()
                .unwrap_or(XllError::ExcelValue(crate::ExcelError::NotAvailable));
            // SAFETY: raw is live and owned by this handle.
            unsafe {
                return_error(self.udf_id, &mut self.raw, &error);
            }
        }
    }
}

struct AsyncCompletionTracker {
    udf_id: &'static str,
    excel_name: &'static str,
    call_id: CallId,
    calculation_id: crate::execution::CalculationId,
    concurrent_calls: usize,
    timer: crate::execution::CallTimer,
    layers: Option<crate::execution::EnteredLayers>,
    completed: bool,
}

impl AsyncCompletionTracker {
    fn new(
        metadata: &CallMetadata,
        timer: crate::execution::CallTimer,
        layers: crate::execution::EnteredLayers,
    ) -> Self {
        Self {
            udf_id: metadata.udf_id,
            excel_name: metadata.excel_name,
            call_id: metadata.call_id,
            calculation_id: metadata.calculation_id,
            concurrent_calls: metadata.concurrent_calls,
            timer,
            layers: Some(layers),
            completed: false,
        }
    }

    fn finish(&mut self, outcome: &CallOutcome<'_>) {
        if !self.completed {
            self.completed = true;
            if let Some(layers) = self.layers.take() {
                layers.exit(outcome);
            }
            let trace_metadata = crate::execution::UdfTraceMetadata {
                udf_id: self.udf_id,
                excel_name: self.excel_name,
                call_id: self.call_id,
                calculation_id: self.calculation_id,
                concurrent_calls: self.concurrent_calls,
            };
            crate::execution::trace(&trace_metadata, outcome);
        }
    }

    fn finish_error(&mut self, error: &XllError) {
        if !self.completed {
            crate::diagnostics::report_no_unwind(self.udf_id, error);
            let outcome = crate::execution::outcome_for_error(error, self.timer.elapsed());
            self.finish(&outcome);
        }
    }
}

impl Drop for AsyncCompletionTracker {
    fn drop(&mut self) {
        if !self.completed {
            let error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
            self.finish_error(&error);
        }
    }
}

/// Runs the synchronous launch portion of a native Excel async UDF.
///
/// # Safety
///
/// `raw_handle` must point to a valid, aligned, Excel-owned `XLOPER12` async
/// handle that remains live for the duration of this call.
#[doc(hidden)]
pub unsafe fn async_udf_boundary_named<S, Start, Fut, T>(
    runtime: &'static Runtime<S>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    S: Send + Sync + 'static,
    Start: FnOnce(Arc<S>, CancellationToken) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: IntoExcelValue + Send + 'static,
{
    let (_export_guard, accepted) = crate::ingress::global_ingress().enter_with(|| {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterExternal);
    });

    if !accepted {
        return;
    }

    let call = match runtime.enter() {
        Ok(call) => call,
        Err(_) => {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
            return;
        }
    };

    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_named_inner(runtime, &call, udf_id, excel_name, raw_handle, start);
        }
    }));

    if result.is_err() {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::Panic);
    }

    drop(call);

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
}

unsafe fn async_udf_boundary_named_inner<S, Start, Fut, T>(
    runtime: &'static Runtime<S>,
    guard: &crate::runtime::CallGuard<'_, S>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    S: Send + Sync + 'static,
    Start: FnOnce(Arc<S>, CancellationToken) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: IntoExcelValue + Send + 'static,
{
    let call_id = runtime.next_call_id();
    let timer = crate::execution::CallTimer::start();
    let started_at = std::time::SystemTime::now();

    let concurrent_calls = guard.concurrent_calls();
    let metadata = CallMetadata {
        udf_id,
        excel_name,
        call_id: CallId::from(call_id),
        calculation_id: runtime.calculation_id(),
        started_at,
        concurrent_calls,
    };
    let configured_layers = runtime
        .layers_if_configured()
        .unwrap_or_else(|| Arc::new(Vec::new()));
    let layers = match crate::execution::EnteredLayers::enter(&configured_layers, &metadata) {
        Ok(layers) => layers,
        Err(error) => {
            crate::diagnostics::report_no_unwind(udf_id, &error);
            // SAFETY: forwarded from this function's raw-handle contract.
            unsafe { return_error(udf_id, raw_handle, &error) };
            return;
        }
    };
    let tracker = Arc::new(Mutex::new(AsyncCompletionTracker::new(
        &metadata, timer, layers,
    )));

    // Excel does not raise CalculationEnded/CalculationCanceled for every
    // programmatic recalculation, so the public token cannot promise complete
    // calculation scoping even though event-driven generations are linearized.
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut handle = match unsafe { OwnedAsyncHandle::from_raw(udf_id, raw_handle) } {
        Ok(handle) => handle,
        Err(error) => {
            tracker.lock().finish_error(&error);
            // SAFETY: forwarded from this function's raw-handle contract.
            unsafe { return_error(udf_id, raw_handle, &error) };
            return;
        }
    };
    let future = catch_unwind(AssertUnwindSafe(|| start(guard.state_arc(), token.clone())))
        .unwrap_or(Err(XllError::Panic));
    match future {
        Ok(future) => {
            let tracker_task = Arc::clone(&tracker);
            let task = async move {
                let evaluated = AssertUnwindSafe(future).catch_unwind().await;
                #[cfg(test)]
                if let Some(hook) = *AFTER_ASYNC_EVALUATION_HOOK.lock() {
                    hook();
                }

                // Linearize delivery vs cancellation using CAS on the delivery state machine.
                if !token.try_start_delivery() {
                    let cancel_error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
                    handle.set_error(cancel_error.clone());
                    tracker_task.lock().finish_error(&cancel_error);
                    return;
                }

                let result = match evaluated {
                    Ok(Ok(value)) => catch_unwind(AssertUnwindSafe(|| {
                        let value = value.into_excel_value()?;
                        AsyncReturnPointer::from_value(value)
                    }))
                    .unwrap_or(Err(XllError::Panic)),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(XllError::Panic),
                };
                let (pointer, computation_error) = match result {
                    Ok(pointer) => (pointer, None),
                    Err(error) => (AsyncReturnPointer::error(&error), Some(error)),
                };
                // SAFETY: both pointers are owned and live for the callback.
                let delivery = unsafe {
                    let delivery = async_return(handle.pointer(), pointer.as_non_null());
                    handle.complete();
                    delivery
                };
                token.finish_delivery();
                match delivery {
                    Ok(()) => match computation_error {
                        Some(error) => tracker_task.lock().finish_error(&error),
                        None => {
                            let outcome = CallOutcome {
                                result: UdfResultKind::Success,
                                error: None,
                                vendor_code: None,
                                duration: timer.elapsed(),
                            };
                            tracker_task.lock().finish(&outcome);
                        }
                    },
                    Err(error) => tracker_task.lock().finish_error(&error),
                }
            };
            if let Err(error) =
                runtime
                    .async_manager()
                    .spawn(metadata.calculation_id.get(), task, cancellation)
            {
                tracker.lock().finish_error(&error);
            }
        }
        Err(error) => {
            handle.set_error(error.clone());
            tracker.lock().finish_error(&error);
        }
    }
}

unsafe fn return_error(udf_id: &'static str, handle: *mut XLOPER12, error: &XllError) {
    let Some(handle) = NonNull::new(handle) else {
        crate::diagnostics::report_no_unwind(
            udf_id,
            &XllError::input(
                "async_handle",
                crate::InputError::Malformed("null async handle"),
            ),
        );
        return;
    };
    let pointer = AsyncReturnPointer::error(error);
    // SAFETY: the RAII-owned return is live for the callback.
    unsafe {
        if let Err(delivery_error) = async_return(handle, pointer.as_non_null()) {
            crate::diagnostics::report_no_unwind(udf_id, &delivery_error);
        }
    }
}

unsafe fn async_return(handle: NonNull<XLOPER12>, result: NonNull<XLOPER12>) -> XllResult<()> {
    let invocation = crate::callback_gate::CallbackInvocationToken::new();
    let callback_gate =
        crate::callback_gate::enter_callback(&invocation).map_err(|suppressed| {
            XllError::ExcelApi {
                function: "xlAsyncReturn(suppressed)",
                code: suppressed.status.raw_code(),
            }
        })?;
    // SAFETY: both XLOPER12 pointers are live for this call. The specialized
    // raw wrapper intentionally does not expose the worker-thread-forbidden
    // xlFree cleanup path.
    let (raw_status, callback_result, invoked) =
        unsafe { xlfn_sys::excel12_async_return(handle, result) };
    let status = crate::ExcelCallbackStatus::from_raw(raw_status);
    callback_gate.observe(status);
    drop(callback_gate);
    let accepted = invoked
        && status == crate::ExcelCallbackStatus::Success
        && callback_result.base_type() == XLTYPE_BOOL
        // SAFETY: XLTYPE_BOOL selects the boolean union field.
        && unsafe { callback_result.value.boolean != 0 };
    if !accepted {
        let error = XllError::ExcelApi {
            function: "xlAsyncReturn",
            code: if !invoked || status == crate::ExcelCallbackStatus::Success {
                -1
            } else {
                status.raw_code()
            },
        };
        return Err(error);
    }
    Ok(())
}
#[cfg(test)]
static AFTER_ASYNC_EVALUATION_HOOK: Mutex<Option<fn()>> = Mutex::new(None);

pub fn cancel_async_calculation<S>(runtime: &Runtime<S>) {
    runtime.cancel_async();
}

pub fn end_async_calculation<S>(runtime: &Runtime<S>) {
    runtime.finish_calculation();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use crate::runtime::tests::TEST_LOCK;
    const TEST_GENERATION: u64 = 1;
    static EVALUATION_BARRIER: Mutex<
        Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
    > = Mutex::new(None);

    struct AsyncTestGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        cleanup: Option<Box<dyn FnOnce()>>,
    }

    impl Drop for AsyncTestGuard {
        fn drop(&mut self) {
            if let Some(cleanup) = self.cleanup.take() {
                let ingress = crate::ingress::global_ingress();
                if ingress.phase() != crate::ingress::PHASE_CLOSED {
                    ingress.begin_close_with(|| {});
                    let _ = ingress.seal_and_drain();
                }
                cleanup();
            }
        }
    }

    fn test_lock() -> AsyncTestGuard {
        AsyncTestGuard {
            _lock: TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner()),
            cleanup: None,
        }
    }

    fn test_lock_for_runtime<S: 'static>(runtime: &'static Runtime<S>) -> AsyncTestGuard {
        AsyncTestGuard {
            _lock: TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner()),
            cleanup: Some(Box::new(move || runtime.release_test_module_lease())),
        }
    }

    fn stop_after_async_evaluation() {
        let barrier = EVALUATION_BARRIER.lock();
        let (reached, release) = barrier.as_ref().expect("evaluation barrier is installed");
        reached.send(()).unwrap();
        release.recv().unwrap();
    }

    fn test_cancellation_source() -> CancellationSource {
        CancellationSource::new(CancellationGuarantee::BestEffort).0
    }

    fn reset_test_callback() -> crate::test_callback::CallbackTestGuard {
        let guard = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        guard
    }

    fn wait_for_async_callback() -> i32 {
        let deadline = Instant::now() + Duration::from_secs(1);
        while crate::test_callback::async_return_calls() == 0 {
            assert!(Instant::now() < deadline, "async callback was not invoked");
            std::thread::yield_now();
        }
        crate::test_callback::last_async_value()
    }

    #[test]
    fn executor_runs_tasks_and_joins_on_close() {
        let manager = AsyncManager::new();
        manager.start(2).unwrap();
        let completed = Arc::new(AtomicBool::new(false));
        let task_completed = Arc::clone(&completed);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    task_completed.store(true, Ordering::Release);
                    done_tx.send(()).unwrap();
                },
                test_cancellation_source(),
            )
            .unwrap();
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(manager.close().issues.is_empty());
        assert!(completed.load(Ordering::Acquire));
    }

    #[test]
    fn cancellation_drops_pending_future_without_running_its_tail() {
        struct DropSignal(Arc<AtomicBool>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let manager = AsyncManager::new();
        manager.start(2).unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = DropSignal(Arc::clone(&dropped));
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    let _signal = signal;
                    std::future::pending::<()>().await;
                },
                test_cancellation_source(),
            )
            .unwrap();
        manager.cancel_generation(TEST_GENERATION);
        assert!(manager.close().issues.is_empty());
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn rejected_spawn_drops_future_after_releasing_manager_state() {
        struct ReentrantRejectedFuture {
            manager: Arc<AsyncManager>,
            dropped: std::sync::mpsc::Sender<()>,
        }

        impl Future for ReentrantRejectedFuture {
            type Output = ();

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                std::task::Poll::Pending
            }
        }

        impl Drop for ReentrantRejectedFuture {
            fn drop(&mut self) {
                self.manager.cancel_generation(TEST_GENERATION);
                self.dropped.send(()).unwrap();
            }
        }

        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        manager.cancel_generation(TEST_GENERATION);
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let future = ReentrantRejectedFuture {
            manager: Arc::clone(&manager),
            dropped: dropped_tx,
        };
        let spawning_manager = Arc::clone(&manager);
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let spawning = std::thread::spawn(move || {
            result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, future, test_cancellation_source()))
                .unwrap();
        });

        assert!(matches!(
            result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        spawning.join().unwrap();
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn spawn_handle_snapshot_is_revalidated_after_generation_advance() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        manager.set_after_spawn_handle_snapshot_hook(Some(Arc::new(move || {
            snapshot_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("spawn snapshot should be released");
        })));

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
                .unwrap();
        });

        snapshot_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn should snapshot the executor handle");
        assert!(manager.advance_generation());
        release_tx.send(()).unwrap();

        assert!(matches!(
            result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        assert!(token.is_cancelled());
        spawning.join().unwrap();
        manager.set_after_spawn_handle_snapshot_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn concurrent_generation_advances_are_serialized() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let hook_barrier = Arc::clone(&barrier);
        manager.set_before_generation_transition_hook(Some(Arc::new(move || {
            hook_barrier.wait();
        })));

        let first_manager = Arc::clone(&manager);
        let first = std::thread::spawn(move || first_manager.advance_generation());
        let second_manager = Arc::clone(&manager);
        let second = std::thread::spawn(move || second_manager.advance_generation());

        assert!(first.join().unwrap());
        assert!(second.join().unwrap());
        assert_eq!(manager.current_generation(), TEST_GENERATION + 2);
        manager
            .spawn(TEST_GENERATION + 2, async {}, test_cancellation_source())
            .unwrap();

        manager.set_before_generation_transition_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn task_scheduling_does_not_hold_manager_state() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        manager.set_before_task_schedule_hook(Some(Arc::new(move || {
            admitted_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("task scheduling should be released");
        })));

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            spawn_result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
                .unwrap();
        });
        admitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task should be admitted before scheduling");

        let cancelling_manager = Arc::clone(&manager);
        let (cancel_done_tx, cancel_done_rx) = std::sync::mpsc::sync_channel(1);
        let cancelling = std::thread::spawn(move || {
            cancelling_manager.cancel_generation(TEST_GENERATION);
            cancel_done_tx.send(()).unwrap();
        });
        cancel_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation should not wait for task scheduling");
        assert!(token.is_cancelled());

        release_tx.send(()).unwrap();
        assert!(
            spawn_result_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_ok()
        );
        spawning.join().unwrap();
        cancelling.join().unwrap();
        manager.set_before_task_schedule_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn close_rejects_a_spawn_using_a_snapshot_handle() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        manager.set_after_spawn_handle_snapshot_hook(Some(Arc::new(move || {
            snapshot_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("spawn snapshot should be released");
        })));

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
                .unwrap();
        });
        snapshot_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn should snapshot the executor handle");

        assert!(manager.close_with_timeout(Duration::from_secs(1)).is_ok());
        release_tx.send(()).unwrap();
        assert!(matches!(
            result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(XllError::Closing)
        ));
        assert!(token.is_cancelled());
        spawning.join().unwrap();
        manager.set_after_spawn_handle_snapshot_hook(None);
        assert!(manager.is_stopped());
    }

    #[test]
    fn close_isolates_panicking_cancellation_waker_and_completes_shutdown() {
        struct PanicWake;

        impl std::task::Wake for PanicWake {
            fn wake(self: Arc<Self>) {
                panic!("injected async close waker panic");
            }

            fn wake_by_ref(self: &Arc<Self>) {
                panic!("injected async close waker panic");
            }
        }

        struct DropSignal(Arc<AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let panic_waker = std::task::Waker::from(Arc::new(PanicWake));
        let mut waiter = Box::pin(token.cancelled());
        assert_eq!(
            waiter
                .as_mut()
                .poll(&mut std::task::Context::from_waker(&panic_waker)),
            std::task::Poll::Pending
        );
        let dropped = Arc::new(AtomicBool::new(false));
        let drop_signal = DropSignal(Arc::clone(&dropped));
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    let _drop_signal = drop_signal;
                    std::future::pending::<()>().await;
                },
                source,
            )
            .unwrap();

        assert!(manager.close().issues.is_empty());
        assert!(token.is_cancelled());
        assert!(dropped.load(Ordering::Acquire));

        // A completed close must leave no orphaned Closing(None) owner.
        assert!(manager.advance_generation());
        manager.start(1).unwrap();
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn close_allows_aborted_future_drop_to_reenter_runtime() {
        let _guard = test_lock();
        struct ReentrantDrop {
            runtime: &'static Runtime<()>,
            dropped: std::sync::mpsc::Sender<()>,
        }

        impl Drop for ReentrantDrop {
            fn drop(&mut self) {
                cancel_async_calculation(self.runtime);
                end_async_calculation(self.runtime);
                self.dropped.send(()).unwrap();
            }
        }

        let runtime: &'static Runtime<()> = Box::leak(Box::new(Runtime::new()));
        runtime.start_async(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let reentrant = ReentrantDrop {
            runtime,
            dropped: dropped_tx,
        };
        runtime
            .async_manager()
            .spawn(
                runtime.calculation_id().get(),
                async move {
                    let _reentrant = reentrant;
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    std::future::pending::<()>().await;
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closed_tx
                .send(
                    runtime
                        .async_manager()
                        .close_with_timeout(Duration::from_secs(2)),
                )
                .unwrap();
        });
        assert!(
            runtime
                .async_manager()
                .wait_for_closing(Duration::from_secs(1))
        );
        assert!(matches!(runtime.start_async(1), Err(XllError::Closing)));
        release_tx.send(()).unwrap();

        dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn close_allows_aborted_layer_cleanup_to_reenter_runtime() {
        struct ReentrantLayer {
            runtime: &'static Runtime<u32>,
            exited: std::sync::mpsc::Sender<()>,
        }
        struct ReentrantLayerGuard {
            runtime: &'static Runtime<u32>,
            exited: std::sync::mpsc::Sender<()>,
        }

        impl crate::UdfLayer for ReentrantLayer {
            fn enter(&self, _: &crate::CallMetadata) -> XllResult<Box<dyn crate::UdfLayerGuard>> {
                Ok(Box::new(ReentrantLayerGuard {
                    runtime: self.runtime,
                    exited: self.exited.clone(),
                }))
            }
        }

        impl crate::UdfLayerGuard for ReentrantLayerGuard {
            fn exit(self: Box<Self>, _: &crate::CallOutcome<'_>) {
                cancel_async_calculation(self.runtime);
                end_async_calculation(self.runtime);
                self.exited.send(()).unwrap();
            }
        }

        let runtime: &'static Runtime<u32> = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let (exited_tx, exited_rx) = std::sync::mpsc::channel();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(
            7_u32,
            vec![Arc::new(ReentrantLayer {
                runtime,
                exited: exited_tx,
            })],
        );
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();
        let _callback_guard = reset_test_callback();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_reentrant_layer_close",
                "TEST.ASYNC.REENTRANT.LAYER.CLOSE",
                &mut handle,
                move |_, _| {
                    Ok(async move {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        std::future::pending::<()>().await;
                        Ok::<_, XllError>(42.0)
                    })
                },
            );
        }
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            closed_tx
                .send(
                    runtime
                        .async_manager()
                        .close_with_timeout(Duration::from_secs(2)),
                )
                .unwrap();
        });
        assert!(
            runtime
                .async_manager()
                .wait_for_closing(Duration::from_secs(1))
        );
        release_tx.send(()).unwrap();

        exited_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn cancellation_token_is_signaled_before_task_abort() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let observed = token.clone();
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    let _token = token;
                    std::future::pending::<()>().await;
                },
                source,
            )
            .unwrap();
        manager.cancel_generation(TEST_GENERATION);
        assert!(observed.is_cancelled());
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn cancelled_generation_rejects_late_spawn_and_next_generation_accepts_work() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        manager.cancel_generation(TEST_GENERATION);

        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));

        let next = TEST_GENERATION + 1;
        assert!(manager.advance_generation());
        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        manager
            .spawn(next, async {}, test_cancellation_source())
            .unwrap();
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn cancelling_new_generation_does_not_cancel_live_work_from_previous_generation() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (old_source, old_token) =
            CancellationSource::new(CancellationGuarantee::CalculationScoped);
        manager
            .spawn(TEST_GENERATION, std::future::pending(), old_source)
            .unwrap();

        let next = TEST_GENERATION + 1;
        assert!(manager.advance_generation());
        let (new_source, new_token) =
            CancellationSource::new(CancellationGuarantee::CalculationScoped);
        manager
            .spawn(next, std::future::pending(), new_source)
            .unwrap();

        manager.cancel_generation(next);
        assert!(new_token.is_cancelled());
        assert!(!old_token.is_cancelled());

        assert!(manager.close().issues.is_empty());
        assert!(old_token.is_cancelled());
    }

    #[test]
    fn spawn_and_cancel_are_linearized_by_generation_admission() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);

        let spawning_manager = Arc::clone(&manager);
        let spawning_barrier = Arc::clone(&barrier);
        let spawning = std::thread::spawn(move || {
            spawning_barrier.wait();
            spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source)
        });

        let cancelling_manager = Arc::clone(&manager);
        let cancelling_barrier = Arc::clone(&barrier);
        let cancelling = std::thread::spawn(move || {
            cancelling_barrier.wait();
            cancelling_manager.cancel_generation(TEST_GENERATION);
        });

        barrier.wait();
        let spawn_result = spawning.join().unwrap();
        cancelling.join().unwrap();

        match spawn_result {
            Ok(()) => assert!(token.is_cancelled()),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable)) => {
                assert!(token.is_cancelled());
            }
            Err(error) => panic!("unexpected spawn result: {error}"),
        }
        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn joined_worker_panic_is_a_cleanup_issue_with_a_stop_certificate() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("injected task panic");
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release_tx.send(()).unwrap();
        let outcome = manager.close();
        assert_eq!(outcome.issues.len(), 1);
        assert_eq!(
            outcome.issues[0].kind,
            crate::CleanupIssueKind::WorkerPanickedAfterJoin
        );
        let _stopped = outcome.certificate;
        assert!(manager.is_stopped());
    }

    #[test]
    fn lone_worker_panic_drops_tasks_left_on_the_queue() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    panic!("injected worker-fatal panic");
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        manager
            .spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            )
            .unwrap();

        let closing = Arc::clone(&manager);
        let closer = std::thread::spawn(move || closing.close());
        release_tx.send(()).unwrap();
        let outcome = closer.join().unwrap();

        assert_eq!(outcome.issues.len(), 1);
        assert_eq!(
            outcome.issues[0].kind,
            crate::CleanupIssueKind::WorkerPanickedAfterJoin
        );
        assert!(manager.is_stopped());
    }

    #[test]
    fn pending_task_limit_is_reserved_atomically() {
        let manager = AsyncManager::new();
        manager.start(2).unwrap();
        for _ in 0..MAX_PENDING {
            manager
                .spawn(
                    TEST_GENERATION,
                    std::future::pending(),
                    test_cancellation_source(),
                )
                .unwrap();
        }
        assert!(matches!(
            manager.spawn(
                TEST_GENERATION,
                std::future::pending(),
                test_cancellation_source(),
            ),
            Err(XllError::Overloaded)
        ));
        manager.cancel_generation(TEST_GENERATION);
        manager.close_with_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn shutdown_timeout_refuses_close_until_blocking_poll_returns() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            manager
                .close_with_timeout(Duration::from_millis(10))
                .is_err()
        );
        release_tx.send(()).unwrap();
        manager.close_with_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn production_close_waits_until_blocking_poll_returns() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        manager
            .spawn(
                TEST_GENERATION,
                async move {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
                test_cancellation_source(),
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let closer_manager = Arc::clone(&manager);
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            assert!(closer_manager.close().issues.is_empty());
            closed_tx.send(()).unwrap();
        });
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());
        release_tx.send(()).unwrap();
        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closer.join().unwrap();
    }

    #[test]
    fn async_handle_payload_is_deep_copied() {
        let mut bytes = vec![1_u8, 2, 3, 4];
        let original = bytes.as_mut_ptr();
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle { data: original },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: raw is a live, well-formed test async handle.
        let mut owned = unsafe { OwnedAsyncHandle::from_raw("test_payload", &mut raw) }.unwrap();
        // SAFETY: the owned value remains XLTYPE_BIG_DATA with a positive size.
        let copied = unsafe { owned.raw.value.big_data.handle.data };
        assert_ne!(copied, original);
        bytes.fill(9);
        assert_eq!(
            // SAFETY: copied points to the owned four-byte payload.
            unsafe { std::slice::from_raw_parts(copied, 4) },
            &[1, 2, 3, 4]
        );
        owned.complete();
    }

    #[test]
    fn async_boundary_returns_completed_value_through_callback() {
        let runtime = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(2).unwrap();

        let _callback_guard = reset_test_callback();
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async",
                "TEST.ASYNC",
                &mut handle,
                |_, token| {
                    assert_eq!(token.guarantee(), CancellationGuarantee::BestEffort);
                    Ok(async { Ok::<_, XllError>(42.0) })
                },
            );
        }
        assert_eq!(wait_for_async_callback(), 42);
        assert_eq!(crate::test_callback::free_calls(), 0);
        assert!(runtime.close_async().issues.is_empty());
    }

    #[test]
    fn async_boundary_reports_handler_failures_to_layers() {
        struct Recorder(std::sync::mpsc::Sender<(UdfResultKind, Option<i32>, usize)>);
        struct RecorderGuard {
            sender: std::sync::mpsc::Sender<(UdfResultKind, Option<i32>, usize)>,
            concurrent_calls: usize,
        }
        impl crate::UdfLayer for Recorder {
            fn enter(
                &self,
                metadata: &crate::CallMetadata,
            ) -> XllResult<Box<dyn crate::UdfLayerGuard>> {
                Ok(Box::new(RecorderGuard {
                    sender: self.0.clone(),
                    concurrent_calls: metadata.concurrent_calls,
                }))
            }
        }
        impl crate::UdfLayerGuard for RecorderGuard {
            fn exit(self: Box<Self>, outcome: &crate::CallOutcome<'_>) {
                self.sender
                    .send((outcome.result, outcome.vendor_code, self.concurrent_calls))
                    .unwrap();
            }
        }

        let runtime = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, vec![Arc::new(Recorder(event_sender))]);
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(2).unwrap();

        let _callback_guard = reset_test_callback();
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_failure",
                "TEST.ASYNC.FAILURE",
                &mut handle,
                |_, _| {
                    Ok(async {
                        Err::<f64, _>(XllError::Native {
                            code: 73,
                            message: "injected async failure".to_owned(),
                        })
                    })
                },
            );
        }
        assert_eq!(wait_for_async_callback(), -1);
        let event = event_receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.0, UdfResultKind::VendorError);
        assert_eq!(event.1, Some(73));
        assert_eq!(event.2, 1);

        assert!(runtime.close_async().issues.is_empty());
    }

    #[test]
    fn async_boundary_records_delivery_rejection_as_failure() {
        struct Recorder(std::sync::mpsc::Sender<UdfResultKind>);
        struct RecorderGuard(std::sync::mpsc::Sender<UdfResultKind>);

        impl crate::UdfLayer for Recorder {
            fn enter(&self, _: &crate::CallMetadata) -> XllResult<Box<dyn crate::UdfLayerGuard>> {
                Ok(Box::new(RecorderGuard(self.0.clone())))
            }
        }

        impl crate::UdfLayerGuard for RecorderGuard {
            fn exit(self: Box<Self>, outcome: &crate::CallOutcome<'_>) {
                self.0.send(outcome.result).unwrap();
            }
        }

        let runtime = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, vec![Arc::new(Recorder(event_sender))]);
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();
        let _callback_guard = reset_test_callback();
        crate::test_callback::set_async_rejected(true);

        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_delivery_failure",
                "TEST.ASYNC.DELIVERY.FAILURE",
                &mut handle,
                |_, _| Ok(async { Ok::<_, XllError>(42.0) }),
            );
        }

        assert_eq!(
            event_receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            UdfResultKind::InternalError
        );
        assert_eq!(crate::test_callback::async_return_calls(), 1);
        assert!(runtime.close_async().issues.is_empty());
    }

    #[test]
    fn async_boundary_returns_error_on_cancellation() {
        let runtime = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(2).unwrap();

        let _callback_guard = reset_test_callback();
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_cancel",
                "TEST.ASYNC.CANCEL",
                &mut handle,
                move |_, _| {
                    let release_rx = release_rx;
                    Ok(async move {
                        let _ = release_rx.recv();
                        Ok::<_, XllError>(123.0)
                    })
                },
            );
        }
        // Cancel all running async tasks. OwnedAsyncHandle::drop should fire and return error to hook.
        cancel_async_calculation(runtime);
        drop(release_tx);
        assert_eq!(wait_for_async_callback(), -1);

        assert!(runtime.close_async().issues.is_empty());
    }

    #[test]
    fn pending_async_cancellation_after_terminal_gate_never_calls_excel() {
        let runtime = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();

        let _callback_guard = reset_test_callback();
        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_terminal_gate",
                "TEST.ASYNC.TERMINAL.GATE",
                &mut handle,
                move |_, _| {
                    Ok(async move {
                        started_tx.send(()).unwrap();
                        std::future::pending::<()>().await;
                        Ok::<_, XllError>(123.0)
                    })
                },
            );
        }
        // Ensure cancellation observes a task that has actually started. If
        // the task were still queued, dropping it would not exercise the
        // OwnedAsyncHandle fallback that must be suppressed by the gate.
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("terminal-gate task did not start");

        let invocation = crate::callback_gate::CallbackInvocationToken::new();
        let callback_gate = crate::callback_gate::enter_callback(&invocation).unwrap();
        callback_gate.observe(crate::ExcelCallbackStatus::Abort);
        drop(callback_gate);
        let callbacks_before_cancel = crate::test_callback::async_return_calls();
        cancel_async_calculation(runtime);
        assert!(runtime.close_async().issues.is_empty());
        assert_eq!(
            crate::test_callback::async_return_calls(),
            callbacks_before_cancel,
            "terminal callback gate must suppress async cancellation fallback while token is active"
        );
        drop(invocation);

        let next_token = crate::callback_gate::CallbackInvocationToken::new();
        assert!(crate::callback_gate::enter_callback(&next_token).is_ok());
    }

    #[test]
    fn cancellation_after_evaluation_does_not_leak_the_return_block() {
        let runtime = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();

        let _callback_guard = reset_test_callback();
        // Record the process-global allocation count only after this runtime
        // owns the module test lease and callback state has been reset. A
        // concurrent return-value test may otherwise free its own block after
        // this test samples the baseline.
        let before = crate::return_value::live_return_blocks();
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *EVALUATION_BARRIER.lock() = Some((reached_tx, release_rx));
        *AFTER_ASYNC_EVALUATION_HOOK.lock() = Some(stop_after_async_evaluation);

        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        // SAFETY: `handle` is a valid, stack-local XLOPER12 constructed above.
        unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_cancel_after_evaluation",
                "TEST.ASYNC.CANCEL.AFTER.EVALUATION",
                &mut handle,
                |_, _| Ok(async { Ok::<_, XllError>("allocated return payload".to_owned()) }),
            );
        }

        reached_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel_async_calculation(runtime);
        release_tx.send(()).unwrap();
        assert_eq!(wait_for_async_callback(), -1);
        assert!(runtime.close_async().issues.is_empty());

        assert_eq!(crate::return_value::live_return_blocks(), before);
        *AFTER_ASYNC_EVALUATION_HOOK.lock() = None;
        *EVALUATION_BARRIER.lock() = None;
    }

    #[test]
    fn advance_generation_does_not_block_on_task_schedule_hook() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        manager.set_before_task_schedule_hook(Some(Arc::new(move || {
            admitted_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("task scheduling should be released");
        })));

        let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            spawn_result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
                .unwrap();
        });
        admitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task should be admitted before scheduling");

        let advancing_manager = Arc::clone(&manager);
        let (advance_done_tx, advance_done_rx) = std::sync::mpsc::sync_channel(1);
        let advancing = std::thread::spawn(move || {
            advance_done_tx
                .send(advancing_manager.advance_generation())
                .unwrap();
        });
        assert!(
            advance_done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("advance_generation should not block while task schedule hook is held")
        );

        release_tx.send(()).unwrap();
        spawn_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        spawning.join().unwrap();
        advancing.join().unwrap();
        manager.set_before_task_schedule_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn spawn_registered_before_close_is_drained_safely() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        manager.set_before_task_schedule_hook(Some(Arc::new(move || {
            registered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("schedule hook should be released");
        })));

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            spawn_result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending(), source))
                .unwrap();
        });
        registered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task should be registered");

        let closing_manager = Arc::clone(&manager);
        let (close_result_tx, close_result_rx) = std::sync::mpsc::sync_channel(1);
        let closing = std::thread::spawn(move || {
            close_result_tx
                .send(closing_manager.close_with_timeout(Duration::from_secs(1)))
                .unwrap();
        });

        release_tx.send(()).unwrap();
        spawning.join().unwrap();
        let _ = spawn_result_rx.recv_timeout(Duration::from_secs(1));

        assert!(
            close_result_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_ok()
        );
        closing.join().unwrap();
        assert!(token.is_cancelled());
        manager.set_before_task_schedule_hook(None);
        assert!(manager.is_stopped());
    }

    #[test]
    fn old_generation_retained_entry_rejected_on_spawn() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        let (source1, _token1) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        manager
            .spawn(TEST_GENERATION, std::future::pending::<()>(), source1)
            .unwrap();
        assert!(manager.advance_generation());
        assert_eq!(manager.current_generation(), TEST_GENERATION + 1);

        let (source2, token2) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let res = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source2);
        assert!(matches!(
            res,
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        assert!(token2.is_cancelled());
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn rejection_priority_old_generation_over_max_pending() {
        let manager = AsyncManager::new();
        manager.start(1).unwrap();
        for _ in 0..MAX_PENDING {
            let (source, _token) =
                CancellationSource::new(CancellationGuarantee::CalculationScoped);
            let _ = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source);
        }
        let (source_curr, token_curr) =
            CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let res_curr = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source_curr);
        assert!(matches!(res_curr, Err(XllError::Overloaded)));
        assert!(!token_curr.is_cancelled());

        assert!(manager.advance_generation());
        let gen2 = TEST_GENERATION + 1;

        for _ in 0..MAX_PENDING {
            let (source, _token) =
                CancellationSource::new(CancellationGuarantee::CalculationScoped);
            let _ = manager.spawn(gen2, std::future::pending::<()>(), source);
        }

        let (source_old, token_old) =
            CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let res_old = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source_old);
        assert!(matches!(
            res_old,
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        assert!(token_old.is_cancelled());

        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn benchmark_concurrent_spawns() {
        let thread_counts = [1, 2, 4, 8, 16, 32];
        let iterations_per_thread = 500;

        for &threads in &thread_counts {
            let manager = Arc::new(AsyncManager::new());
            manager.start(4).unwrap();
            let current_gen = manager.current_generation();
            let start = Instant::now();

            let accepted = Arc::new(AtomicUsize::new(0));
            let overloaded = Arc::new(AtomicUsize::new(0));
            let other_errors = Arc::new(AtomicUsize::new(0));

            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let mgr = Arc::clone(&manager);
                    let accepted = Arc::clone(&accepted);
                    let overloaded = Arc::clone(&overloaded);
                    let other_errors = Arc::clone(&other_errors);
                    std::thread::spawn(move || {
                        for _ in 0..iterations_per_thread {
                            let (source, _token) =
                                CancellationSource::new(CancellationGuarantee::BestEffort);
                            match mgr.spawn(current_gen, async {}, source) {
                                Ok(()) => {
                                    accepted.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(XllError::Overloaded) => {
                                    overloaded.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(_) => {
                                    other_errors.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    })
                })
                .collect();

            for h in handles {
                h.join().unwrap();
            }

            let elapsed = start.elapsed();
            let total_ops = threads * iterations_per_thread;
            let acc = accepted.load(Ordering::SeqCst);
            let ov = overloaded.load(Ordering::SeqCst);
            let err = other_errors.load(Ordering::SeqCst);
            let ratio = if total_ops > 0 {
                (acc as f64 / total_ops as f64) * 100.0
            } else {
                0.0
            };
            let accepted_ops_per_sec = acc as f64 / elapsed.as_secs_f64();
            println!(
                "Async spawn bench: {threads} threads, {total_ops} attempts, accepted: {acc}, overloaded: {ov}, errors: {err}, ratio: {ratio:.1}%, accepted_throughput: {accepted_ops_per_sec:.2} accepted_ops/sec",
            );

            assert!(manager.close().issues.is_empty());
        }
    }

    #[test]
    fn test_generation_state_sharded_removal_and_task_count() {
        let state = GenerationState::new(1);
        let (abort, _) = AbortHandle::new_pair();
        for id in 1..=100 {
            let index = task_shard(id);
            let (cancellation, _) = CancellationSource::new(CancellationGuarantee::BestEffort);
            state.shards[index].tasks.lock().insert(
                id,
                TaskControl {
                    abort: abort.clone(),
                    cancellation,
                },
            );
            state.task_count.fetch_add(1, Ordering::AcqRel);
        }

        assert_eq!(state.task_count.load(Ordering::Acquire), 100);

        // Remove 40 tasks via remove_task
        for id in 1..=40 {
            assert!(state.remove_task(id));
        }
        assert_eq!(state.task_count.load(Ordering::Acquire), 60);

        // Drain remaining 60 tasks
        let drained = state.drain_tasks();
        assert_eq!(drained.len(), 60);
        assert_eq!(state.task_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn spawn_and_advance_linearization_case_a_advance_closes_before_admission() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

        manager.set_after_generation_snapshot_hook(Some(Arc::new(move || {
            snapshot_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("snapshot hook should be released");
        })));

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            spawn_result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source))
                .unwrap();
        });

        snapshot_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn should snapshot current generation");

        let advancing_manager = Arc::clone(&manager);
        let (advance_result_tx, advance_result_rx) = std::sync::mpsc::sync_channel(1);
        let advancing = std::thread::spawn(move || {
            advance_result_tx
                .send(advancing_manager.advance_generation())
                .unwrap();
        });

        assert!(
            advance_result_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
        );
        release_tx.send(()).unwrap();

        let spawn_res = spawn_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            spawn_res,
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        assert!(token.is_cancelled());

        spawning.join().unwrap();
        advancing.join().unwrap();
        manager.set_after_generation_snapshot_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn spawn_and_advance_linearization_case_b_admission_holds_advance() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

        manager.set_after_generation_admission_hook(Some(Arc::new(move || {
            admitted_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("admission hook should be released");
        })));

        let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            spawn_result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source))
                .unwrap();
        });

        admitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn should enter admission");

        let advancing_manager = Arc::clone(&manager);
        let (advance_result_tx, advance_result_rx) = std::sync::mpsc::sync_channel(1);
        let advancing = std::thread::spawn(move || {
            advance_result_tx
                .send(advancing_manager.advance_generation())
                .unwrap();
        });

        // advance_generation should block on wait_for_idle while admission is held
        assert!(
            advance_result_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );

        release_tx.send(()).unwrap();
        assert!(
            spawn_result_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_ok()
        );
        assert!(
            advance_result_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
        );

        spawning.join().unwrap();
        advancing.join().unwrap();
        manager.set_after_generation_admission_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn spawn_and_cancel_linearization_case_a_spawn_admitted_first() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

        manager.set_after_generation_admission_hook(Some(Arc::new(move || {
            admitted_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("admission hook should be released");
        })));

        let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let spawning_manager = Arc::clone(&manager);
        let (spawn_result_tx, spawn_result_rx) = std::sync::mpsc::sync_channel(1);
        let spawning = std::thread::spawn(move || {
            spawn_result_tx
                .send(spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source))
                .unwrap();
        });

        admitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn should enter admission");

        let cancelling_manager = Arc::clone(&manager);
        let (cancel_result_tx, cancel_result_rx) = std::sync::mpsc::sync_channel(1);
        let cancelling = std::thread::spawn(move || {
            cancelling_manager.cancel_generation(TEST_GENERATION);
            cancel_result_tx.send(()).unwrap();
        });

        // cancel_generation should block waiting for admission idle
        assert!(
            cancel_result_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );

        release_tx.send(()).unwrap();
        assert!(
            spawn_result_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_ok()
        );
        cancel_result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        spawning.join().unwrap();
        cancelling.join().unwrap();
        assert!(token.is_cancelled());
        manager.set_after_generation_admission_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn spawn_and_cancel_linearization_case_b_cancel_closed_first() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();
        manager.cancel_generation(TEST_GENERATION);

        let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let res = manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source);
        assert!(matches!(
            res,
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));
        assert!(token.is_cancelled());
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn advance_does_not_hold_control_mutex_while_waiting_for_idle() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();

        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

        manager.set_after_generation_admission_hook(Some(Arc::new(move || {
            admitted_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("admission hook should be released");
        })));

        let (source, _token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let spawning = std::thread::spawn(move || {
            spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source)
        });

        admitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task should be admitted");

        let advancing_manager = Arc::clone(&manager);
        let (advance_done_tx, advance_done_rx) = std::sync::mpsc::sync_channel(1);
        let advancing = std::thread::spawn(move || {
            advance_done_tx
                .send(advancing_manager.advance_generation())
                .unwrap();
        });

        std::thread::sleep(Duration::from_millis(50));

        let executor_inner = match &*manager.state.lock() {
            ExecutorState::Running(executor) => Arc::clone(&executor.handle.inner),
            _ => panic!("executor should be running"),
        };

        assert!(
            executor_inner.control.try_lock().is_some(),
            "advance_generation must release control mutex while waiting for admission idle"
        );

        release_tx.send(()).unwrap();
        assert!(
            advance_done_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
        );

        spawning.join().unwrap().unwrap();
        advancing.join().unwrap();
        manager.set_after_generation_admission_hook(None);
        assert!(manager.close().issues.is_empty());
    }

    #[test]
    fn close_preempts_in_progress_advance_generation() {
        let manager = Arc::new(AsyncManager::new());
        manager.start(1).unwrap();

        let (admitted_tx, admitted_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));

        manager.set_after_generation_admission_hook(Some(Arc::new(move || {
            admitted_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .expect("admission hook should be released");
        })));

        let (source, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
        let spawning_manager = Arc::clone(&manager);
        let spawning = std::thread::spawn(move || {
            spawning_manager.spawn(TEST_GENERATION, std::future::pending::<()>(), source)
        });

        admitted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task should be admitted");

        let advancing_manager = Arc::clone(&manager);
        let (advance_done_tx, advance_done_rx) = std::sync::mpsc::sync_channel(1);
        let advancing = std::thread::spawn(move || {
            advance_done_tx
                .send(advancing_manager.advance_generation())
                .unwrap();
        });

        std::thread::sleep(Duration::from_millis(50));

        let closing_manager = Arc::clone(&manager);
        let (close_done_tx, close_done_rx) = std::sync::mpsc::sync_channel(1);
        let closing = std::thread::spawn(move || {
            close_done_tx.send(closing_manager.close()).unwrap();
        });

        std::thread::sleep(Duration::from_millis(50));

        let executor_inner = match &*manager.state.lock() {
            ExecutorState::Closing(executor) => {
                executor.as_ref().map(|exec| Arc::clone(&exec.handle.inner))
            }
            _ => None,
        };

        if let Some(inner) = executor_inner {
            assert!(
                inner.closing.load(Ordering::Acquire),
                "close must set closing atomic mirror even while advance is waiting"
            );
        }

        release_tx.send(()).unwrap();

        assert!(
            !advance_done_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
        );

        let close_report = close_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(close_report.issues.is_empty());
        assert!(token.is_cancelled());

        spawning.join().unwrap().unwrap();
        advancing.join().unwrap();
        closing.join().unwrap();
        manager.set_after_generation_admission_hook(None);
    }

    #[test]
    fn async_udf_boundary_catches_unhandled_panics_at_ffi_boundary() {
        struct PanickingLayer;

        impl crate::execution::UdfLayer for PanickingLayer {
            fn enter(
                &self,
                _: &CallMetadata,
            ) -> XllResult<Box<dyn crate::execution::UdfLayerGuard>> {
                panic!("injected layer panic in outer boundary");
            }
        }

        let runtime = Box::leak(Box::new(Runtime::new()));
        let _guard = test_lock_for_runtime(runtime);
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(1_u32, vec![Arc::new(PanickingLayer)]);
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        runtime.start_async(1).unwrap();

        let mut bytes = vec![1_u8, 2, 3, 4];
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: bytes.as_mut_ptr(),
                    },
                    byte_count: bytes.len() as i32,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };

        // SAFETY: handle is a valid, stack-local XLOPER12 constructed above.
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            async_udf_boundary_named(
                runtime,
                "test_async_panic_boundary",
                "TEST.ASYNC.PANIC",
                &mut handle,
                |_, _| Ok(async { Ok::<_, XllError>(42.0) }),
            );
        }));

        assert!(
            result.is_ok(),
            "async_udf_boundary_named must catch panics at the FFI boundary"
        );
        assert!(runtime.close_async().issues.is_empty());
    }
}
