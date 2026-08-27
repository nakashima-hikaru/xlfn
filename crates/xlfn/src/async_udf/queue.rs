use std::sync::atomic::{AtomicU64, Ordering};

use async_task::Runnable;
use crossbeam_deque::{Injector, Stealer, Worker};
use crossbeam_utils::sync::Unparker;
use xlfn_kernel::drain_gate::DrainGate;

/// Concurrency invariants for `RunnableQueue`:
///
/// - **Q1 (Schedule Gate Admission)**: New `Runnable` instances can only be enqueued while
///   `schedule_admission` is OPEN. Callers must acquire a drain gate permit before pushing
///   to the global `injector`.
/// - **Q2 (Seal Linearization)**: Once `seal_and_wake_all()` (or `schedule_admission.seal_and_wait()`)
///   completes, no subsequent `Runnable` can ever be admitted or pushed into the `injector`
///   or any worker local queue.
/// - **Q3 (Two-Stage Shutdown Separation)**: `closing == true` (in `ExecutorShared`) disables
///   *spawn admission* for new tasks, but does *NOT* seal `schedule_admission`. In-flight tasks
///   and abort/cancellation wakers can continue scheduling `Runnable`s until all active tasks
///   drain (`active == 0`). Only then is `schedule_admission` sealed during final close.
/// - **Q4 (Liveness / No Lost Wakeups)**: Whenever work is enqueued or batch-stolen into a local
///   queue with extra tasks, if sleeping workers exist in `idle_workers`, at least one worker
///   is unparked and guaranteed to observe the work.
/// - **Q5 (Worker Panic Task Preservation)**: If a worker thread panics during task execution,
///   its `WorkerExitGuard` returns all remaining `Runnable`s in its local queue back to the
///   `injector` and wakes remaining workers so no queued tasks are permanently stranded.
pub(crate) struct RunnableQueue {
    pub(crate) injector: Injector<Runnable>,
    stealers: Box<[Stealer<Runnable>]>,
    schedule_admission: DrainGate,
    pub(crate) idle_workers: AtomicU64,
    unparkers: Box<[Unparker]>,
}

impl RunnableQueue {
    pub(crate) fn new(stealers: Box<[Stealer<Runnable>]>, unparkers: Box<[Unparker]>) -> Self {
        Self {
            injector: Injector::new(),
            stealers,
            schedule_admission: DrainGate::new_open(),
            idle_workers: AtomicU64::new(0),
            unparkers,
        }
    }

    pub(crate) fn schedule(&self, runnable: Runnable) {
        let Ok(_permit) = self.schedule_admission.try_enter() else {
            drop(runnable);
            return;
        };
        self.injector.push(runnable);
        self.wake_one();
    }

    pub(crate) fn wake_one(&self) {
        let mut idle = self.idle_workers.load(Ordering::Acquire);
        while idle != 0 {
            let worker_index = idle.trailing_zeros() as usize;
            let mask = 1u64 << worker_index;
            match self.idle_workers.compare_exchange_weak(
                idle,
                idle & !mask,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if let Some(unparker) = self.unparkers.get(worker_index) {
                        unparker.unpark();
                    }
                    return;
                }
                Err(actual) => idle = actual,
            }
        }
    }

    pub(crate) fn wake_all(&self) {
        self.idle_workers.store(0, Ordering::Release);
        for unparker in self.unparkers.iter() {
            unparker.unpark();
        }
    }

    pub(crate) fn seal_and_wake_all(&self) {
        self.schedule_admission.seal_and_wait();
        self.wake_all();
    }

    pub(crate) fn is_sealed(&self) -> bool {
        self.schedule_admission.is_sealed()
    }

    pub(crate) fn steal_injector_batch_and_pop(
        &self,
        local: &Worker<Runnable>,
    ) -> Option<Runnable> {
        loop {
            match self.injector.steal_batch_and_pop(local) {
                crossbeam_deque::Steal::Success(runnable) => {
                    if !local.is_empty() {
                        self.wake_one();
                    }
                    return Some(runnable);
                }
                crossbeam_deque::Steal::Empty => return None,
                crossbeam_deque::Steal::Retry => {}
            }
        }
    }

    pub(crate) fn steal_peer(&self, worker_index: usize) -> Option<Runnable> {
        let count = self.stealers.len();
        if count <= 1 {
            return None;
        }
        for offset in 1..count {
            let peer_index = (worker_index + offset) % count;
            loop {
                match self.stealers[peer_index].steal() {
                    crossbeam_deque::Steal::Success(runnable) => return Some(runnable),
                    crossbeam_deque::Steal::Empty => break,
                    crossbeam_deque::Steal::Retry => {}
                }
            }
        }
        None
    }

    /// Drains any abandoned `Runnable`s left in the global injector and worker stealers.
    ///
    /// # Safety / Preconditions
    /// This method is intended solely for failure recovery (`fatal_worker_failure == true`)
    /// and requires that all workers have already exited (`live_workers == 0`) so that no
    /// concurrent access to local queues occurs.
    pub(crate) fn drain_abandoned(&self) -> Option<Runnable> {
        loop {
            match self.injector.steal() {
                crossbeam_deque::Steal::Success(runnable) => return Some(runnable),
                crossbeam_deque::Steal::Empty => break,
                crossbeam_deque::Steal::Retry => {}
            }
        }
        for stealer in self.stealers.iter() {
            loop {
                match stealer.steal() {
                    crossbeam_deque::Steal::Success(runnable) => return Some(runnable),
                    crossbeam_deque::Steal::Empty => break,
                    crossbeam_deque::Steal::Retry => {}
                }
            }
        }
        None
    }
}
