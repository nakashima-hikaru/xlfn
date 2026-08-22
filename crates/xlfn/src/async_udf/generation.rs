use super::task::TaskControl;
use parking_lot::{Condvar, Mutex};
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

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

pub(crate) const ADMISSION_CLOSED: usize = 1usize << (usize::BITS - 1);
pub(crate) const ADMISSION_COUNT_MASK: usize = ADMISSION_CLOSED - 1;

pub(crate) struct GenerationAdmission {
    pub(crate) state: AtomicUsize,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
}

impl GenerationAdmission {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    pub(crate) fn try_enter(&self) -> Option<AdmissionPermit<'_>> {
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

    pub(crate) fn close(&self) {
        self.state.fetch_or(ADMISSION_CLOSED, Ordering::AcqRel);
    }

    pub(crate) fn wait_for_idle(&self) {
        let mut guard = self.wait_lock.lock();
        while self.state.load(Ordering::Acquire) & ADMISSION_COUNT_MASK != 0 {
            self.idle.wait(&mut guard);
        }
    }
}

pub(crate) struct AdmissionPermit<'a> {
    pub(crate) admission: &'a GenerationAdmission,
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

pub(crate) struct GenerationState {
    pub(crate) id: u64,
    pub(crate) admission: GenerationAdmission,
    pub(crate) task_count: AtomicUsize,
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
            admission: GenerationAdmission::new(),
            task_count: AtomicUsize::new(0),
            shards,
        }
    }

    pub(crate) fn remove_task(&self, id: u64) -> bool {
        let index = task_shard(id);
        let mut tasks = self.shards[index].tasks.lock();
        if tasks.remove(&id).is_some() {
            self.task_count.fetch_sub(1, Ordering::AcqRel);
            true
        } else {
            false
        }
    }

    pub(crate) fn drain_tasks(&self) -> Vec<TaskControl> {
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

pub(crate) struct ExecutorControl {
    pub(crate) phase: ControlPhase,
    pub(crate) generations: FxHashMap<u64, triomphe::Arc<GenerationState>>,
}
