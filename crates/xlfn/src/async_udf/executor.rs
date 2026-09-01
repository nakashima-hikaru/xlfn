use super::generation::{ControlPhase, ExecutorControl, GenerationState, task_shard};
use super::queue::RunnableQueue;
use super::task::{ActiveReservation, SpawnRejection, TaskControl};
use super::worker::{cancelled_calculation_error, run_executor};
use crate::addin::AsyncWorkerCount;
use crate::cancellation::CancellationSource;
use crate::diagnostics::id::DiagnosticId;
use crate::error::DomainErrorCode;
use crate::shutdown::CleanupIssueKind;
use crate::{XllError, XllResult};
use crossbeam_utils::sync::Parker;
use futures_util::future::{AbortHandle, Abortable};
use parking_lot::{Condvar, Mutex};
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::{Duration, Instant};

pub(crate) struct Executor {
    pub(crate) shared: Box<ExecutorShared>,
    pub(crate) workers: Vec<JoinHandle<()>>,
}

/// Non-owning executor capability used by workers and detached async tasks.
///
/// The unique `Box<ExecutorShared>` remains in `Executor`. Shutdown drains all
/// tasks and joins every worker before reclaiming that Box.
#[derive(Clone, Copy)]
pub(crate) struct ExecutorPtr(NonNull<ExecutorShared>);

impl ExecutorPtr {
    pub(crate) fn from_ref(shared: &ExecutorShared) -> Self {
        Self(NonNull::from(shared))
    }

    pub(crate) fn get(self) -> &'static ExecutorShared {
        // SAFETY: construction and use are confined to the executor temporal
        // protocol documented on this capability and `Executor::finish_close`.
        unsafe { self.0.as_ref() }
    }
}

// SAFETY: ExecutorShared is thread-safe. Its unique owner drains tasks and
// joins workers before reclamation, so transferred capabilities remain valid.
unsafe impl Send for ExecutorPtr {}
unsafe impl Sync for ExecutorPtr {}

/// Shared executor state.
///
/// Ownership invariants:
///
/// - Each `Executor` uniquely owns one stable `Box<ExecutorShared>`.
/// - `AsyncManager` publishes only a non-owning pointer protected by spawn
///   admission; publication never participates in ownership.
/// - Workers and active tasks carry non-owning capabilities. Global active
///   accounting and worker joins must complete before the Box is reclaimed.
/// - `Executor` exclusively owns the worker JoinHandles.
/// - The runnable queue is terminated explicitly with `queue.seal_and_wake_all()`.
/// - After sealing the queue, workers drain normally; if no workers remain,
///   `Executor::drain_after_worker_failure` explicitly drains queued runnables.
///
/// Lifecycle invariants:
/// I1. `current`'s `GenerationState` always exists in `control.generations` until `ControlPhase::Closing`.
/// I2. When `control.phase == ControlPhase::Running`, `current.admission` is the admission authority for the current generation.
/// I3. When `control.phase == ControlPhase::Advancing { from, to }`, `current.id == from` and `current.admission` is closed.
/// I4. After `control.phase == ControlPhase::Closing`, no new `GenerationState` is ever published to `current`.
/// I5. A `GenerationState` may be removed from `control.generations` only when `generation != next` and `task_count == 0`.
///
/// Two-Stage Shutdown & Queue Invariants (Q1–Q5):
/// - Q1: New `Runnable`s can only be enqueued while `queue.schedule_admission` is OPEN.
/// - Q2: After `queue.seal_and_wake_all()` completes, no new `Runnable` can enter the injector or local queues.
/// - Q3: `closing == true` terminates *spawn admission* for new tasks, but does NOT seal `schedule_admission`.
///   Aborting/canceling active tasks may re-schedule `Runnable`s until all active tasks complete (`active == 0`).
///   Only then is `finish_close()` or `drain_after_worker_failure()` allowed to seal `schedule_admission`.
/// - Q4: Sleeping workers in `idle_workers` are woken whenever work is enqueued or batch-stolen.
/// - Q5: Worker panic recovers all remaining tasks from its local queue back to the global injector.
pub(crate) struct ExecutorShared {
    pub(crate) queue: RunnableQueue,
    pub(crate) next_id: AtomicU64,
    pub(crate) active: AtomicUsize,
    pub(crate) live_workers: AtomicUsize,
    pub(crate) fatal_worker_failure: AtomicBool,
    /// Monotonic fast-path mirror of `ExecutorControl::phase == ControlPhase::Closing`.
    ///
    /// Lifecycle transitions are authoritative under `control`;
    /// spawn reads only this atomic.
    pub(crate) closing: AtomicBool,
    pub(crate) current: std::sync::atomic::AtomicPtr<GenerationState>,
    /// Cold lifecycle state. Never acquired by spawn/completion.
    pub(crate) control: Mutex<ExecutorControl>,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
    pub(crate) observer: crate::shutdown_trace::ObservationSink,
    #[cfg(test)]
    pub(crate) before_task_schedule_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_generation_snapshot_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_generation_admission_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Executor {
    pub(crate) fn start(worker_count: usize, generation: u64) -> XllResult<Self> {
        Self::start_internal(worker_count, generation, None)
    }

    #[cfg(test)]
    pub(crate) fn start_with_failure_at(
        worker_count: usize,
        generation: u64,
        fail_at: Option<usize>,
    ) -> XllResult<Self> {
        Self::start_internal(worker_count, generation, fail_at)
    }

    fn start_internal(
        worker_count: usize,
        generation: u64,
        fail_at: Option<usize>,
    ) -> XllResult<Self> {
        if !(1..=AsyncWorkerCount::MAX).contains(&worker_count) {
            return Err(XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            });
        }
        let mut workers_local = Vec::with_capacity(worker_count);
        let mut stealers = Vec::with_capacity(worker_count);
        let mut unparkers = Vec::with_capacity(worker_count);

        for _ in 0..worker_count {
            let worker = crossbeam_deque::Worker::new_fifo();
            stealers.push(worker.stealer());
            let parker = Parker::new();
            unparkers.push(parker.unparker().clone());
            workers_local.push((worker, parker));
        }

        let queue = RunnableQueue::new(
            stealers.into_boxed_slice(),
            unparkers.into_boxed_slice(),
        );
        let initial_generation = Box::new(GenerationState::new(generation));
        let initial_pointer = NonNull::from(initial_generation.as_ref());
        let shared = Box::new(ExecutorShared {
            queue,
            next_id: AtomicU64::new(1),
            active: AtomicUsize::new(0),
            live_workers: AtomicUsize::new(0),
            fatal_worker_failure: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            current: std::sync::atomic::AtomicPtr::new(initial_pointer.as_ptr()),
            control: Mutex::new(ExecutorControl {
                phase: ControlPhase::Running,
                generations: [(generation, initial_generation)].into_iter().collect(),
            }),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
            observer: crate::shutdown_trace::ObservationSink::new(),
            #[cfg(test)]
            before_task_schedule_hook: Mutex::new(None),
            #[cfg(test)]
            after_generation_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            after_generation_admission_hook: Mutex::new(None),
        });
        let shared_pointer = ExecutorPtr::from_ref(shared.as_ref());
        let mut workers = scopeguard::guard(
            Vec::<JoinHandle<()>>::with_capacity(worker_count),
            move |mut workers| {
                shared_pointer.get().queue.seal_and_wake_all();
                while let Some(worker) = workers.pop() {
                    drop(worker.join());
                }
            },
        );
        for (index, (local_worker, parker)) in workers_local.into_iter().enumerate() {
            if fail_at == Some(index) {
                return Err(XllError::Internal {
                    diagnostic_id: DiagnosticId::ASYNC_SPAWN,
                });
            }
            shared.live_workers.fetch_add(1, Ordering::Release);
            let worker_shared = ExecutorPtr::from_ref(shared.as_ref());
            let worker = thread::Builder::new()
                .name(format!("xlfn-async-{index}"))
                .spawn(move || {
                    run_executor(index, worker_shared, local_worker, parker);
                });
            let worker = match worker {
                Ok(worker) => worker,
                Err(_) => {
                    let _ = xlfn_kernel::invariant::checked_atomic_dec(&shared.live_workers);
                    return Err(XllError::Internal {
                        diagnostic_id: DiagnosticId::ASYNC_SPAWN,
                    });
                }
            };
            workers.push(worker);
        }
        let workers = scopeguard::ScopeGuard::into_inner(workers);
        Ok(Self { shared, workers })
    }

    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.shared.observer.set_trace_sink(trace);
    }

    pub(crate) fn wait_for_idle(&self) -> bool {
        let mut guard = self.shared.wait_lock.lock();
        while self.shared.active.load(Ordering::Acquire) != 0 {
            if self.shared.fatal_worker_failure.load(Ordering::Acquire)
                && self.shared.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            self.shared.idle.wait(&mut guard);
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn wait_for_idle_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.shared.wait_lock.lock();
        while self.shared.active.load(Ordering::Acquire) != 0 {
            if self.shared.fatal_worker_failure.load(Ordering::Acquire)
                && self.shared.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.shared.idle.wait_for(&mut guard, deadline - now);
        }
        true
    }

    pub(crate) fn drain_after_worker_failure(&self) -> bool {
        debug_assert!(
            self.shared.fatal_worker_failure.load(Ordering::Acquire),
            "drain_after_worker_failure requires a fatal worker failure"
        );
        debug_assert_eq!(
            self.shared.live_workers.load(Ordering::Acquire),
            0,
            "drain_after_worker_failure requires all workers to have exited"
        );
        self.shared.queue.seal_and_wake_all();
        while let Some(runnable) = self.shared.queue.drain_abandoned() {
            drop(runnable);
        }
        self.shared.active.load(Ordering::Acquire) == 0
    }

    pub(crate) fn finish_close(mut self) -> Vec<crate::shutdown::CleanupIssue> {
        self.shared.queue.seal_and_wake_all();
        let mut issues = Vec::new();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                issues.push(crate::shutdown::CleanupIssue {
                    component: "async worker",
                    kind: CleanupIssueKind::WorkerPanickedAfterJoin,
                    error: XllError::Panic,
                });
            }
        }
        issues
    }
}

impl ExecutorShared {
    pub(crate) fn spawn<F>(
        &self,
        generation: u64,
        future: F,
        cancellation: CancellationSource,
    ) -> Result<(), SpawnRejection<F>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.closing.load(Ordering::Acquire) {
            return Err(SpawnRejection {
                error: XllError::Closing,
                future,
                cancellation,
                cancel: true,
            });
        }

        let current_pointer = self.current.load(Ordering::Acquire);
        if current_pointer.is_null() {
            xlfn_kernel::invariant::fail_stop();
        }
        // SAFETY: the pointer names a generation Box owned by `control`.
        // Generation admission is closed and drained before a non-current,
        // task-free generation can be reclaimed.
        let current = unsafe { &*current_pointer };

        #[cfg(test)]
        {
            let hook = self.after_generation_snapshot_hook.lock().clone();
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

        let Some(admission) = current.admission.enter().ok() else {
            let error = if self.closing.load(Ordering::Acquire) {
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
            let hook = self.after_generation_admission_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }

        let Some(reservation) = ActiveReservation::try_acquire(self) else {
            drop(admission);
            return Err(SpawnRejection {
                error: XllError::Overloaded,
                future,
                cancellation,
                cancel: false,
            });
        };

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
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

        let completion = reservation.commit(self, current, id);

        drop(admission);
        self.observer
            .record(crate::shutdown_trace::ShutdownEvent::StartAsyncTask);

        let wrapped = async move {
            let _completion = completion;
            let result = Abortable::new(future, registration).await;
            _completion.observation.finished(result.is_ok());
        };
        #[cfg(test)]
        {
            let hook = self.before_task_schedule_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let shared = ExecutorPtr::from_ref(self);
        let schedule = move |runnable| {
            shared.get().queue.schedule(runnable);
        };
        let (runnable, task) = async_task::spawn(wrapped, schedule);
        task.detach();
        runnable.schedule();
        Ok(())
    }

    pub(crate) fn cancel_generation(&self, generation: u64) -> Vec<TaskControl> {
        let control = self.control.lock();
        let Some(state) = control.generations.get(&generation) else {
            return Vec::new();
        };
        debug_assert_eq!(state.id, generation);
        state.admission.close_and_wait_begin().wait();
        state.drain_tasks()
    }

    pub(crate) fn advance_generation(&self, next: u64) -> bool {
        let old = {
            let mut control = self.control.lock();
            match control.phase {
                ControlPhase::Running => {}
                ControlPhase::Closing => return false,
                ControlPhase::Advancing { .. } => {
                    debug_assert!(false, "concurrent executor generation transition");
                    return false;
                }
            }

            let old_pointer = self.current.load(Ordering::Acquire);
            if old_pointer.is_null() {
                xlfn_kernel::invariant::fail_stop();
            }
            // SAFETY: generation-transition serialization prevents removal of
            // the current generation while its admission is being drained.
            let old = unsafe { &*old_pointer };
            old.admission.begin_close();
            control.phase = ControlPhase::Advancing {
                from: old.id,
                to: next,
            };
            old
        };

        old.admission.close_and_wait_begin().wait();

        let mut control = self.control.lock();
        match control.phase {
            ControlPhase::Closing => return false,
            ControlPhase::Advancing { from, to } if from == old.id && to == next => {}
            ControlPhase::Running | ControlPhase::Advancing { .. } => {
                debug_assert!(false, "executor generation transition state diverged");
                return false;
            }
        }

        let next_pointer = {
            let next_generation = control
                .generations
                .entry(next)
                .or_insert_with(|| Box::new(GenerationState::new(next)));
            NonNull::from(next_generation.as_ref())
        };

        self.current.store(next_pointer.as_ptr(), Ordering::Release);

        control.generations.retain(|generation, state| {
            *generation == next || state.task_count.load(Ordering::Acquire) != 0
        });

        control.phase = ControlPhase::Running;
        true
    }

    pub(crate) fn request_close(&self) -> Vec<TaskControl> {
        let mut control = self.control.lock();

        if matches!(control.phase, ControlPhase::Closing) {
            return Vec::new();
        }

        self.closing.store(true, Ordering::Release);
        control.phase = ControlPhase::Closing;

        for generation in control.generations.values() {
            generation.admission.begin_close();
        }
        for generation in control.generations.values() {
            generation.admission.close_and_wait_begin().wait();
        }

        let mut tasks = Vec::new();
        for generation in control.generations.values() {
            tasks.extend(generation.drain_tasks());
        }
        tasks
    }
}
