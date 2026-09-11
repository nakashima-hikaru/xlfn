use super::task::TaskControl;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use xlfn_kernel::published_owner::PublishedOwner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlPhase {
    Running,
    Advancing { from: u64, to: u64 },
    Closing,
}

pub(crate) const TASK_SHARDS: usize = 32;

pub(crate) fn task_shard(id: u64) -> usize {
    (id as usize) & (TASK_SHARDS - 1)
}

pub(crate) struct TaskShard {
    pub(crate) tasks: Mutex<FxHashMap<u64, TaskControl>>,
}

pub(crate) struct GenerationState {
    pub(crate) id: u64,
    pub(crate) admission: xlfn_kernel::operation_gate::OperationGate,
    // Allocation hint only; shard mutexes publish and own the actual task map.
    pub(crate) task_count: AtomicUsize,
    /// Reservations and completion guards, including canceled tasks whose
    /// controls have already been drained. This is the reclamation authority.
    pub(crate) pins: AtomicUsize,
    pub(crate) shards: Box<[TaskShard]>,
}

impl GenerationState {
    pub(crate) fn new(id: u64) -> Self {
        let shards = (0..TASK_SHARDS)
            .map(|_| TaskShard {
                tasks: Mutex::new(FxHashMap::default()),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            id,
            admission: xlfn_kernel::operation_gate::OperationGate::new(),
            task_count: AtomicUsize::new(0),
            pins: AtomicUsize::new(0),
            shards,
        }
    }

    pub(crate) fn remove_task(&self, id: u64) -> bool {
        let index = task_shard(id);
        let removed = {
            let mut tasks = self.shards[index].tasks.lock();
            let removed = tasks.remove(&id);
            if removed.is_some() {
                let _ = xlfn_kernel::invariant::checked_atomic_dec_relaxed(&self.task_count);
            }
            removed
        };
        let existed = removed.is_some();
        // CancellationSource destruction can drop or wake user wakers.
        drop(removed);
        existed
    }

    pub(crate) fn drain_tasks(&self) -> Vec<TaskControl> {
        // Admission is drained before cancellation reaches this method.
        // Completions can still remove controls, so this is an upper bound.
        // Drain directly into one buffer rather than allocating and copying
        // an intermediate Vec for each shard. Callers drop/wake the returned
        // controls only after releasing the executor control lock.
        let mut result = Vec::with_capacity(self.task_count.load(Ordering::Relaxed));
        for shard in self.shards.iter() {
            let mut tasks = shard.tasks.lock();
            let count = tasks.len();
            result.extend(tasks.drain().map(|(_, task)| task));
            if count != 0 {
                let _ = xlfn_kernel::invariant::checked_atomic_sub_relaxed(&self.task_count, count);
            }
        }
        result
    }
}

/// Non-owning generation capability. The executor owns the Box and only
/// retires non-current generations with no pins under its publication lock.
pub(crate) struct GenerationPin(NonNull<GenerationState>);

impl GenerationPin {
    /// # Safety
    /// The caller must hold the executor's generation-publication lock and
    /// keep the executor alive until this pin is released.
    pub(crate) unsafe fn acquire(state: &GenerationState) -> Self {
        // The publication lock already acquires initialization and excludes
        // reclamation while this lifetime reservation is added.
        state
            .pins
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pins| {
                pins.checked_add(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        Self(NonNull::from(state))
    }

    pub(crate) fn pointer(&self) -> NonNull<GenerationState> {
        self.0
    }

    pub(crate) fn get(&self) -> &GenerationState {
        // SAFETY: the pin prevents this allocation from being reclaimed.
        unsafe { self.0.as_ref() }
    }
}

impl Drop for GenerationPin {
    fn drop(&mut self) {
        // No generation access may follow the release: a concurrent advance
        // can immediately reclaim the Box when this was its final pin. Its
        // Acquire zero observation must see every pin holder's prior accesses.
        let _ = xlfn_kernel::invariant::checked_atomic_dec_release(&self.get().pins);
    }
}

// SAFETY: GenerationState is Sync, and the pin follows its user across threads.
unsafe impl Send for GenerationPin {}

pub(crate) struct ExecutorControl {
    pub(crate) phase: ControlPhase,
    /// Unique generation owners. Moving hash-table entries never retags the
    /// published allocations while reservations and task completions use them.
    pub(crate) generations: FxHashMap<u64, PublishedOwner<GenerationState>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miri_generation_final_pin_release_can_race_owner_reclamation() {
        for id in 0..16 {
            let owner = PublishedOwner::new(GenerationState::new(id));
            // SAFETY: the owner is private until both pins are acquired;
            // reclamation below waits for their terminal publication.
            let first = unsafe { GenerationPin::acquire(&owner) };
            // SAFETY: the owner is still private and both pins are drained below.
            let second = unsafe { GenerationPin::acquire(&owner) };
            let values = [AtomicUsize::new(0), AtomicUsize::new(0)];
            std::thread::scope(|scope| {
                let first_value = &values[0];
                let first = scope.spawn(move || {
                    first_value.store(1, Ordering::Relaxed);
                    drop(first);
                });
                let second_value = &values[1];
                let second = scope.spawn(move || {
                    second_value.store(2, Ordering::Relaxed);
                    drop(second);
                });
                while owner.pins.load(Ordering::Acquire) != 0 {
                    std::thread::yield_now();
                }
                // The zero observation, not joining either thread, must
                // acquire both holders' writes through the release sequence.
                assert_eq!(values[0].load(Ordering::Relaxed), 1);
                assert_eq!(values[1].load(Ordering::Relaxed), 2);
                drop(owner);
                first.join().unwrap();
                second.join().unwrap();
            });
        }
    }
}
