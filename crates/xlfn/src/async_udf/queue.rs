use std::sync::atomic::{AtomicU64, Ordering};

use async_task::Runnable;
use crossbeam_deque::{Injector, Stealer, Worker};
use crossbeam_utils::sync::Unparker;
use xlfn_kernel::drain_gate::DrainGate;

// Keep the memory-order handshake shared with the Loom model. Every work
// publication and idle announcement must participate in the same RMW order.
trait IdleWorkerMask {
    fn publish_and_observe(&self, bits: u64) -> u64;
    fn try_claim(&self, current: u64, next: u64) -> Result<u64, u64>;
}

impl IdleWorkerMask for AtomicU64 {
    fn publish_and_observe(&self, bits: u64) -> u64 {
        self.fetch_or(bits, Ordering::AcqRel)
    }

    fn try_claim(&self, current: u64, next: u64) -> Result<u64, u64> {
        self.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
    }
}

fn claim_idle_worker(mask: &impl IdleWorkerMask) -> Option<usize> {
    // A load can miss a worker's concurrent idle announcement while its
    // queue recheck misses our push (store buffering). An unconditional
    // RMW joins the announcement's modification order: either we observe
    // its bit and unpark it, or its later AcqRel announcement observes our
    // release and must see the queued work before parking.
    let mut idle = mask.publish_and_observe(0);
    while idle != 0 {
        let worker_index = idle.trailing_zeros() as usize;
        match mask.try_claim(idle, idle & !(1u64 << worker_index)) {
            Ok(_) => return Some(worker_index),
            Err(actual) => idle = actual,
        }
    }
    None
}

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
        if let Some(worker_index) = claim_idle_worker(&self.idle_workers)
            && let Some(unparker) = self.unparkers.get(worker_index)
        {
            unparker.unpark();
        }
    }

    pub(crate) fn announce_idle(&self, worker_bit: u64) {
        self.idle_workers.publish_and_observe(worker_bit);
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

#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86"))))]
mod tests {
    use super::{IdleWorkerMask, claim_idle_worker};
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    impl IdleWorkerMask for AtomicU64 {
        fn publish_and_observe(&self, bits: u64) -> u64 {
            self.fetch_or(bits, Ordering::AcqRel)
        }

        fn try_claim(&self, current: u64, next: u64) -> Result<u64, u64> {
            self.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
        }
    }

    #[test]
    fn every_supported_worker_has_a_distinct_claimable_idle_bit() {
        let mask = std::sync::atomic::AtomicU64::new(u64::MAX);
        for index in 0..crate::AsyncWorkerCount::MAX {
            assert_eq!(claim_idle_worker(&mask), Some(index));
        }
        assert_eq!(claim_idle_worker(&mask), None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_queue_publication_cannot_leave_work_behind_a_sleeping_worker() {
        loom::model(|| {
            let idle = Arc::new(AtomicU64::new(0));
            let queued = Arc::new(AtomicBool::new(false));
            let worker_idle = Arc::clone(&idle);
            let worker_queue = Arc::clone(&queued);
            let worker = loom::thread::spawn(move || {
                if worker_queue.load(Ordering::Acquire) {
                    return true;
                }
                worker_idle.publish_and_observe(1);
                worker_queue.load(Ordering::Acquire)
            });
            queued.store(true, Ordering::Release);
            let notified = claim_idle_worker(&*idle).is_some();
            let found_work = worker.join().unwrap();
            assert!(
                found_work || notified,
                "a worker must see queued work or receive a park token"
            );
        });
    }
}
