use super::task::TaskControl;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::atomic::AtomicUsize;

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
            admission: xlfn_kernel::operation_gate::OperationGate::new(),
            task_count: AtomicUsize::new(0),
            shards,
        }
    }

    pub(crate) fn remove_task(&self, id: u64) -> bool {
        let index = task_shard(id);
        let mut tasks = self.shards[index].tasks.lock();
        if tasks.remove(&id).is_some() {
            let _ = xlfn_kernel::invariant::checked_atomic_dec(&self.task_count);
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
                let _ = xlfn_kernel::invariant::checked_atomic_sub(&self.task_count, count);
            }
        }
        result
    }
}

pub(crate) struct ExecutorControl {
    pub(crate) phase: ControlPhase,
    pub(crate) generations: FxHashMap<u64, triomphe::Arc<GenerationState>>,
}
