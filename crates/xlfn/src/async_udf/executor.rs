use super::generation::{
    ControlPhase, ExecutorControl, GenerationPin, GenerationState, task_shard,
};
use super::queue::RunnableQueue;
use super::task::{ActiveReservation, TaskControl, TrackedFuture};
use super::worker::{cancelled_calculation_error, run_executor};
use crate::addin::AsyncWorkerCount;
use crate::cancellation::CancellationSource;
use crate::diagnostics::id::DiagnosticId;
use crate::error::DomainErrorCode;
#[cfg(feature = "handles")]
use crate::generation::RuntimeGeneration;
#[cfg(feature = "handles")]
use crate::handle::GenerationLeaseBrand;
use crate::shutdown::CleanupIssueKind;
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use crossbeam_utils::sync::Parker;
use futures_util::future::AbortHandle;
#[cfg(feature = "handles")]
use std::marker::PhantomData;
#[cfg(feature = "handles")]
use std::pin::Pin;
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
#[cfg(test)]
use std::time::{Duration, Instant};
use xlfn_kernel::operation_gate::OperationGate;
use xlfn_kernel::published_owner::PublishedOwner;

pub(crate) struct Executor {
    pub(crate) shared: PublishedOwner<ExecutorShared>,
    pub(crate) workers: Vec<JoinHandle<()>>,
}

/// Non-owning executor capability used by workers and detached async tasks.
///
/// The unique allocation remains in `Executor`. Shutdown drains all tasks and
/// scheduler callbacks, then joins every worker before reclaiming it.
#[derive(Clone, Copy)]
pub(crate) struct ExecutorPtr(NonNull<ExecutorShared>);

impl ExecutorPtr {
    pub(crate) fn from_ref(shared: &ExecutorShared) -> Self {
        Self(NonNull::from(shared))
    }

    /// Borrows the shared executor state.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the backing `Executor` has not finished
    /// its close sequence and reclaimed `ExecutorShared`.
    #[inline]
    pub(crate) unsafe fn get(self) -> &'static ExecutorShared {
        // SAFETY: Delegated to the caller's guarantee that Executor is still alive.
        unsafe { self.0.as_ref() }
    }
}

// SAFETY: ExecutorShared is thread-safe. Its unique owner drains tasks and
// joins workers before reclamation, so transferred capabilities remain valid.
unsafe impl Send for ExecutorPtr {}
// SAFETY: ExecutorShared is thread-safe and immutable borrows can be shared.
unsafe impl Sync for ExecutorPtr {}

/// Shared executor state.
///
/// Ownership invariants:
///
/// - Each `Executor` uniquely owns one `PublishedOwner<ExecutorShared>` whose
///   movements do not invalidate published readers.
/// - `AsyncManager` publishes only a non-owning pointer protected by spawn
///   admission; publication never participates in ownership.
/// - Workers and active tasks carry non-owning capabilities. Global active
///   accounting, scheduler callbacks, and worker joins must complete before
///   the Box is reclaimed.
/// - `Executor` exclusively owns the worker JoinHandles.
/// - The runnable queue is terminated explicitly with `queue.seal_and_wake_all()`.
/// - After sealing the queue, workers drain normally; the last failed worker
///   explicitly drains abandoned runnables.
///
/// Lifecycle invariants:
/// I1. `current`'s `GenerationState` always exists in `control.generations` until `ControlPhase::Closing`.
/// I2. When `control.phase == ControlPhase::Running`, `current.admission` is the admission authority for the current generation.
/// I3. When `control.phase == ControlPhase::Advancing { from, to }`, `current.id == from` and `current.admission` is closed.
/// I4. After `control.phase == ControlPhase::Closing`, no new `GenerationState` is ever published to `current`.
/// I5. Generation publication, pin acquisition, and reclamation serialize on
/// `generation_publication`. Only non-current generations with zero pins may
/// be reclaimed; canceled task controls do not determine task lifetimes.
///
/// Two-Stage Shutdown & Queue Invariants (Q1–Q7):
/// - Q1: New `Runnable`s can only be enqueued while `queue.schedule_admission` is OPEN.
/// - Q2: After `queue.seal_and_wake_all()` completes, no new `Runnable` can enter the injector or local queues.
/// - Q3: `closing == true` terminates *spawn admission* for new tasks, but does NOT seal `schedule_admission`.
///   Aborting/canceling active tasks may re-schedule `Runnable`s until all active tasks complete (`active == 0`).
///   Final close seals `schedule_admission` after active tasks drain. Failure
///   recovery may seal it earlier because late scheduling destroys runnables.
/// - Q4: Sleeping workers in `idle_workers` are woken whenever work is enqueued or batch-stolen.
/// - Q5: Worker panic recovers all remaining tasks from its local queue back to the global injector.
/// - Q6: Worker failure closes spawn admission and cancels admitted tasks. The
///   last exiting worker drains queued runnables; scheduling after that exit
///   destroys the runnable directly under callback lifetime admission.
/// - Q7: Every scheduler callback retains `scheduler_callbacks` through
///   synchronous destruction and final task release. The owner drains this
///   gate after active reaches zero and before reclaiming shared state.
pub(crate) struct ExecutorShared {
    pub(crate) queue: RunnableQueue,
    scheduler_callbacks: OperationGate,
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
    /// Protects only pointer publication and lifetime-pin acquisition. Never
    /// held while waiting for admission or executing task/user code.
    generation_publication: Mutex<()>,
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
    #[cfg(test)]
    pub(crate) before_scheduler_admission_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_scheduler_drop_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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

        let queue = RunnableQueue::new(stealers.into_boxed_slice(), unparkers.into_boxed_slice());
        let initial_generation = PublishedOwner::new(GenerationState::new(generation));
        let initial_pointer = NonNull::from(initial_generation.as_ref());
        let shared = PublishedOwner::new(ExecutorShared {
            queue,
            scheduler_callbacks: OperationGate::new(),
            next_id: AtomicU64::new(1),
            active: AtomicUsize::new(0),
            live_workers: AtomicUsize::new(0),
            fatal_worker_failure: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            current: std::sync::atomic::AtomicPtr::new(initial_pointer.as_ptr()),
            generation_publication: Mutex::new(()),
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
            #[cfg(test)]
            before_scheduler_admission_hook: Mutex::new(None),
            #[cfg(test)]
            after_scheduler_drop_hook: Mutex::new(None),
        });
        let shared_pointer = ExecutorPtr::from_ref(shared.as_ref());
        let mut workers = scopeguard::guard(
            Vec::<JoinHandle<()>>::with_capacity(worker_count),
            move |mut workers| {
                // SAFETY: workers are joined before the Box<ExecutorShared> is reclaimed.
                unsafe { shared_pointer.get() }.queue.seal_and_wake_all();
                while let Some(worker) = workers.pop() {
                    let _ = crate::panic_boundary::contain_panic(worker.join());
                }
            },
        );
        for (index, (local_worker, parker)) in workers_local.into_iter().enumerate() {
            if fail_at == Some(index) {
                return Err(XllError::Internal {
                    diagnostic_id: DiagnosticId::ASYNC_SPAWN,
                });
            }
            // Reserve before spawning; thread creation and executor publication
            // provide initialization ordering, not this worker count.
            shared.live_workers.fetch_add(1, Ordering::Relaxed);
            let worker_shared = ExecutorPtr::from_ref(shared.as_ref());
            let worker = thread::Builder::new()
                .name(format!("xlfn-async-{index}"))
                .spawn(move || {
                    run_executor(index, worker_shared, local_worker, parker);
                });
            let worker = match worker {
                Ok(worker) => worker,
                Err(_) => {
                    let _ =
                        xlfn_kernel::invariant::checked_atomic_dec_release(&shared.live_workers);
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

    pub(crate) fn wait_for_idle(&self) {
        let mut guard = self.shared.wait_lock.lock();
        while self.shared.active.load(Ordering::Acquire) != 0 {
            // The last failed worker drains queued tasks. A scheduler already
            // holding a runnable can still be executing on an external waker
            // thread, so live_workers == 0 does not certify final destruction.
            self.shared.idle.wait(&mut guard);
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_for_idle_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.shared.wait_lock.lock();
        while self.shared.active.load(Ordering::Acquire) != 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.shared.idle.wait_for(&mut guard, deadline - now);
        }
        true
    }

    pub(crate) fn finish_close(mut self) -> Vec<crate::shutdown::CleanupIssue> {
        self.shared
            .scheduler_callbacks
            .close_and_wait_begin()
            .wait();
        self.shared.queue.seal_and_wake_all();
        self.join_workers()
    }

    fn join_workers(&mut self) -> Vec<crate::shutdown::CleanupIssue> {
        let mut issues = Vec::new();
        for worker in self.workers.drain(..) {
            if crate::panic_boundary::contain_panic(worker.join()).is_err() {
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

impl Drop for Executor {
    fn drop(&mut self) {
        if self.workers.is_empty() {
            self.shared
                .scheduler_callbacks
                .close_and_wait_begin()
                .wait();
            return;
        }
        // Owning this allocation also owns the shutdown obligation. An
        // unwinding caller or a forgotten explicit close must never detach
        // workers that still hold non-owning executor/generation pointers.
        let tasks = self.shared.request_close();
        super::worker::cancel_tasks(tasks);
        self.wait_for_idle();
        self.shared
            .scheduler_callbacks
            .close_and_wait_begin()
            .wait();
        self.shared.queue.seal_and_wake_all();
        let _ = self.join_workers();
    }
}

pub(crate) struct SpawnReservation<'a> {
    pub(crate) shared: ExecutorPtr,
    pub(crate) task_id: u64,
    // Drop admission before its generation pin, then release executor activity.
    pub(crate) admission: xlfn_kernel::operation_gate::OperationGuard<'a>,
    pub(crate) generation: GenerationPin,
    pub(crate) reservation: ActiveReservation<'a>,
}

/// Narrow lifetime brand for the generated async handle path.
///
/// The scope carries no runtime borrow. Its lifetime is a compile-time token
/// that the executor validates at the one point where the scoped task future
/// is erased to the executor's existing `'static` task type.
#[cfg(feature = "handles")]
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct AsyncTaskScope<'generation> {
    generation: RuntimeGeneration,
    _brand: PhantomData<&'generation GenerationLeaseBrand>,
}

/// Generated-user-code builder for the narrow async handle path.
///
/// A trait method, rather than a general borrowed-future callback, gives the
/// generated implementation an explicit late-bound lifetime for each task
/// scope while keeping the rest of the executor `'static`-task based.
#[cfg(feature = "handles")]
#[doc(hidden)]
pub trait HandleScopedBuilder<T> {
    fn build<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> Pin<Box<dyn Future<Output = crate::XllResult<T>> + Send + 'generation>>;
}

#[cfg(feature = "handles")]
pub(crate) trait HandleScopedTaskBuilder {
    fn build_task<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> ScopedTaskFuture<'generation>;
}

#[cfg(feature = "handles")]
impl<'generation> AsyncTaskScope<'generation> {
    pub(crate) fn new(generation: RuntimeGeneration, _: &'generation GenerationLeaseBrand) -> Self {
        Self {
            generation,
            _brand: PhantomData,
        }
    }

    pub(crate) const fn generation(self) -> RuntimeGeneration {
        self.generation
    }
}

#[cfg(feature = "handles")]
pub(crate) type ScopedTaskFuture<'generation> =
    Pin<Box<dyn Future<Output = ()> + Send + 'generation>>;
#[cfg(feature = "handles")]
type ErasedTaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Erases the compile-time handle-generation brand after the generated
/// future has been constrained to contain only task-owned data.
///
/// # Safety
///
/// The caller must ensure that the future contains no borrow from the caller's
/// stack whose validity depends on the scope brand. Generated async contexts
/// may borrow the `ExecutionLease` and cancellation token moved into the same
/// future; those references are therefore task-self-contained. The only
/// external lifetime marker is the zero-sized `AsyncTaskScope`/`HandleLease`
/// brand, while the object lifetime is protected independently by
/// `RawObjectLeaseGuard`. The async shutdown pipeline drains these tasks
/// before tearing down the formula-handle service and its arena.
#[cfg(feature = "handles")]
unsafe fn erase_scoped_task_future<'generation>(
    future: ScopedTaskFuture<'generation>,
) -> ErasedTaskFuture {
    // SAFETY: upheld by `HandleScopedBuilder`'s late-bound method and the
    // `'static` builder bound, plus the shutdown ordering documented above.
    unsafe { std::mem::transmute::<ScopedTaskFuture<'generation>, ErasedTaskFuture>(future) }
}

impl<'a> SpawnReservation<'a> {
    pub(crate) fn commit<F>(self, future: F, cancellation: CancellationSource)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // SAFETY: the pin protects the generation throughout commit and is
        // transferred to completion before admission is released.
        let generation = unsafe { self.generation.pointer().as_ref() };
        // SAFETY: self.admission protects the executor from reclamation until committed.
        let shared = unsafe { self.shared.get() };
        let (abort, registration) = AbortHandle::new_pair();

        let index = task_shard(self.task_id);
        {
            let mut tasks = generation.shards[index].tasks.lock();
            let previous = tasks.insert(
                self.task_id,
                TaskControl {
                    abort,
                    cancellation,
                },
            );
            debug_assert!(previous.is_none(), "task ID must be unique per generation");
        }

        let completion = self.reservation.commit(self.generation, self.task_id);

        drop(self.admission);
        shared
            .observer
            .record(crate::shutdown_trace::ShutdownEvent::StartAsyncTask);

        let wrapped = TrackedFuture::new(future, registration, completion);
        #[cfg(test)]
        {
            let hook = shared.before_task_schedule_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let shared_ptr = self.shared;
        let schedule = move |runnable| {
            // SAFETY: this runnable retains its task's completion guard until
            // the callback enters its own lifetime gate below.
            let shared = unsafe { shared_ptr.get() };
            #[cfg(test)]
            {
                let hook = shared.before_scheduler_admission_hook.lock().clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            // SAFETY: this runnable's active task retains the executor until
            // admission succeeds. Its owner then seals and drains this same
            // gate after active reaches zero, retaining it through the owned
            // guard's final release notification before shared reclamation.
            let Ok(_callback) = (unsafe { shared.scheduler_callbacks.enter_owned() }) else {
                // Callback admission is sealed only after all task futures
                // have been destroyed, when no runnable can call us again.
                xlfn_kernel::invariant::fail_stop();
            };
            if shared.fatal_worker_failure.load(Ordering::Acquire)
                && shared.live_workers.load(Ordering::Acquire) == 0
            {
                // A reservation may finish scheduling after the last failed
                // worker drained its queue. Reclaim it here rather than admit
                // work to a queue with no consumer. NoUnwindFuture protects
                // async-task's otherwise aborting future-destruction boundary.
                drop(runnable);
                #[cfg(test)]
                {
                    let hook = shared.after_scheduler_drop_hook.lock().clone();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
            } else {
                shared.queue.schedule(runnable);
            }
        };
        let (runnable, task) = async_task::spawn(wrapped, schedule);
        task.detach();
        runnable.schedule();
    }

    #[cfg(feature = "handles")]
    pub(crate) fn commit_handle_scoped<B>(
        self,
        runtime_generation: RuntimeGeneration,
        build: B,
        cancellation: CancellationSource,
    ) where
        B: HandleScopedTaskBuilder + Send + 'static,
    {
        let brand = GenerationLeaseBrand;
        let scope = AsyncTaskScope::new(runtime_generation, &brand);
        let future = build.build_task(scope);
        // The scope brand is intentionally erased once, at this executor
        // boundary. All task state remains owned or guarded by its pin.
        // SAFETY: `HandleScopedTaskBuilder` is late-bound over the private
        // scope brand, and the shutdown protocol drains this task before the
        // pinned handle service can be reclaimed.
        let future = unsafe { erase_scoped_task_future(future) };
        self.commit(future, cancellation);
    }
}

impl ExecutorShared {
    pub(crate) fn reserve_spawn(
        &self,
        generation: u64,
    ) -> Result<SpawnReservation<'_>, (XllError, bool)> {
        if self.closing.load(Ordering::Acquire) {
            return Err((XllError::Closing, true));
        }
        if self.fatal_worker_failure.load(Ordering::Acquire)
            || self.live_workers.load(Ordering::Acquire) == 0
        {
            return Err((
                XllError::Internal {
                    diagnostic_id: DiagnosticId::ASYNC_SPAWN,
                },
                true,
            ));
        }

        let generation_pin = {
            let _publication = self.generation_publication.lock();
            let current_pointer = self.current.load(Ordering::Acquire);
            if current_pointer.is_null() {
                xlfn_kernel::invariant::fail_stop();
            }
            // SAFETY: publication and reclamation hold this same mutex. The
            // caller's executor admission protects ExecutorShared itself.
            let current = unsafe { &*current_pointer };
            // SAFETY: the publication lock and caller's executor admission
            // satisfy the pin's allocation and lifetime preconditions.
            unsafe { GenerationPin::acquire(current) }
        };
        // SAFETY: generation_pin remains live until after admission, and is
        // transferred to the returned reservation/completion guard.
        let current = unsafe { generation_pin.pointer().as_ref() };

        #[cfg(test)]
        {
            let hook = self.after_generation_snapshot_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }

        if current.id != generation {
            return Err((cancelled_calculation_error(), true));
        }

        let Some(admission) = current.admission.enter().ok() else {
            let error = if self.closing.load(Ordering::Acquire) {
                XllError::Closing
            } else {
                cancelled_calculation_error()
            };

            return Err((error, true));
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
            return Err((XllError::Overloaded, false));
        };

        let task_id = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| (XllError::Overloaded, false))?;

        Ok(SpawnReservation {
            shared: ExecutorPtr::from_ref(self),
            generation: generation_pin,
            task_id,
            reservation,
            admission,
        })
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
                .or_insert_with(|| PublishedOwner::new(GenerationState::new(next)));
            NonNull::from(next_generation.as_ref())
        };

        let _publication = self.generation_publication.lock();
        self.current.store(next_pointer.as_ptr(), Ordering::Release);

        control.generations.retain(|generation, state| {
            *generation == next || state.pins.load(Ordering::Acquire) != 0
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
