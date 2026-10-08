use std::sync::atomic::{AtomicU64, Ordering};

use async_task::Runnable;
use crossbeam_deque::{Injector, Stealer, Worker};
use crossbeam_utils::sync::Unparker;
use xlfn_kernel::drain_gate::DrainGate;

// Keep the memory-order handshake shared with the Loom model. Every work
// publication and idle announcement must participate in the same RMW order.
trait IdlePollerMask {
    fn publish_work(&self) -> u64;
    fn announce_idle(&self, bits: u64);
    fn try_claim(&self, current: u64, next: u64) -> Result<u64, u64>;
}

impl IdlePollerMask for AtomicU64 {
    fn publish_work(&self) -> u64 {
        self.fetch_or(0, Ordering::Release)
    }

    fn announce_idle(&self, bits: u64) {
        self.fetch_or(bits, Ordering::Acquire);
    }

    fn try_claim(&self, current: u64, next: u64) -> Result<u64, u64> {
        self.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
    }
}

fn claim_idle_poller(mask: &impl IdlePollerMask) -> Option<usize> {
    // A load can miss a worker's concurrent idle announcement while its
    // queue recheck misses our push (store buffering). An unconditional
    // RMW joins the announcement's modification order: either we observe
    // its bit and unpark it, or its later Acquire announcement observes our
    // Release and must see the queued work before parking. Claim/clear RMWs
    // only change bits and preserve that release sequence even when Relaxed.
    let mut idle = mask.publish_work();
    while idle != 0 {
        let worker_index = idle.trailing_zeros() as usize;
        match mask.try_claim(idle, idle & !(1u64 << worker_index)) {
            Ok(_) => return Some(worker_index),
            Err(actual) => idle = actual,
        }
    }
    None
}

/// Runnable publication and poller wake protocol.
///
/// Schedule admission drains before a queue is sealed. The runtime drains
/// its task registry before asking this executor to stop; this queue knows
/// only runnable ownership and poller wakeups. A failed final poller seals
/// publication early and destroys any abandoned runnables.
pub(crate) struct RunnableQueue {
    pub(crate) injector: Injector<Runnable>,
    stealers: Box<[Stealer<Runnable>]>,
    schedule_admission: DrainGate,
    pub(crate) idle_pollers: AtomicU64,
    unparkers: Box<[Unparker]>,
}

impl RunnableQueue {
    pub(crate) fn new(stealers: Box<[Stealer<Runnable>]>, unparkers: Box<[Unparker]>) -> Self {
        Self {
            injector: Injector::new(),
            stealers,
            schedule_admission: DrainGate::new_open(),
            idle_pollers: AtomicU64::new(0),
            unparkers,
        }
    }

    pub(crate) fn schedule(&self, runnable: Runnable) {
        let Ok(_permit) = self.schedule_admission.try_enter() else {
            super::drop_runnable(runnable);
            return;
        };
        self.injector.push(runnable);
        self.wake_one();
    }

    pub(crate) fn wake_one(&self) {
        if let Some(worker_index) = claim_idle_poller(&self.idle_pollers)
            && let Some(unparker) = self.unparkers.get(worker_index)
        {
            unparker.unpark();
        }
    }

    pub(crate) fn announce_idle(&self, worker_bit: u64) {
        self.idle_pollers.announce_idle(worker_bit);
    }

    pub(crate) fn wake_all(&self) {
        self.idle_pollers.store(0, Ordering::Release);
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

    /// Drains abandoned runnables from the injector and poller stealers.
    ///
    /// # Safety / Preconditions
    /// All pollers must already have exited, and scheduling must be sealed.
    /// This prevents concurrent ownership of a local queue during recovery.
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
    use super::{IdlePollerMask, claim_idle_poller};
    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    impl IdlePollerMask for AtomicU64 {
        fn publish_work(&self) -> u64 {
            self.fetch_or(0, Ordering::Release)
        }

        fn announce_idle(&self, bits: u64) {
            self.fetch_or(bits, Ordering::Acquire);
        }

        fn try_claim(&self, current: u64, next: u64) -> Result<u64, u64> {
            self.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
        }
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
                if worker_queue.load(Ordering::Relaxed) {
                    return true;
                }
                worker_idle.announce_idle(1);
                worker_queue.load(Ordering::Relaxed)
            });
            // Model queue contents without their own synchronization so the
            // test specifically requires the idle-mask publication handshake.
            queued.store(true, Ordering::Relaxed);
            let notified = claim_idle_poller(&*idle).is_some();
            let found_work = worker.join().unwrap();
            assert!(
                found_work || notified,
                "a worker must see queued work or receive a park token"
            );
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_idle_mask_clear_preserves_the_work_release_sequence() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.check(|| {
            let idle = Arc::new(AtomicU64::new(2));
            let queued = Arc::new(AtomicBool::new(false));
            let worker_idle = Arc::clone(&idle);
            let worker_queue = Arc::clone(&queued);
            let worker = loom::thread::spawn(move || {
                worker_idle.announce_idle(1);
                worker_queue.load(Ordering::Relaxed)
            });
            let clearing_idle = Arc::clone(&idle);
            let clearing_worker = loom::thread::spawn(move || {
                clearing_idle.fetch_and(!2, Ordering::Relaxed);
            });
            queued.store(true, Ordering::Relaxed);
            let notified = claim_idle_poller(&*idle);
            let found_work = worker.join().unwrap();
            assert!(found_work || notified == Some(0));
            clearing_worker.join().unwrap();
        });
    }
}
