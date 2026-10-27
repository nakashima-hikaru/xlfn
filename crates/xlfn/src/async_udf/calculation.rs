use super::task::{TaskControl, TaskControlBatch};
use crate::sync::Mutex;
use crossbeam_utils::CachePadded;
use rustc_hash::FxHashMap;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use xlfn_kernel::operation_gate::{GateClosed, OperationGuard, TerminationWaitGuard};

/// Identity of one calculation admission domain. Zero is never published.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct CalculationEpoch(std::num::NonZeroU64);

impl CalculationEpoch {
    pub(crate) const INITIAL: Self = Self(std::num::NonZeroU64::new(1).unwrap());

    pub(crate) const fn new(value: u64) -> Option<Self> {
        match std::num::NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn checked_next(self) -> Option<Self> {
        Self::new(self.get().checked_add(1)?)
    }
}

impl From<CalculationEpoch> for crate::execution::CalculationId {
    fn from(epoch: CalculationEpoch) -> Self {
        Self::new(epoch.get())
    }
}

const TASK_SHARDS: usize = 32;

fn task_shard(id: u64) -> usize {
    (id as usize) & (TASK_SHARDS - 1)
}

struct TaskShard {
    tasks: Mutex<FxHashMap<u64, TaskControl>>,
}

pub(crate) struct CalculationState {
    pub(crate) epoch: CalculationEpoch,
    admission: xlfn_kernel::operation_gate::OperationGate,
    /// Preparation snapshots, reservations, and completion guards, including
    /// canceled tasks whose controls have already been drained. This is the
    /// calculation allocation's reclamation authority.
    pins: AtomicUsize,
    shards: Box<[CachePadded<TaskShard>]>,
}

impl CalculationState {
    pub(crate) fn new(epoch: CalculationEpoch) -> Self {
        let shards = (0..TASK_SHARDS)
            .map(|_| {
                CachePadded::new(TaskShard {
                    tasks: Mutex::new(FxHashMap::default()),
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            epoch,
            admission: xlfn_kernel::operation_gate::OperationGate::new(),
            pins: AtomicUsize::new(0),
            shards,
        }
    }

    pub(crate) fn remove_task(&self, id: u64) -> bool {
        let index = task_shard(id);
        let removed = self.shards[index].tasks.lock().remove(&id);
        let existed = removed.is_some();
        // Source destruction can wake executor/user wakers. Contain their
        // destructors after releasing the shard lock.
        let _ =
            crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| drop(removed)));
        existed
    }

    pub(crate) fn enter_admission(&self) -> Result<CalculationAdmission<'_>, GateClosed> {
        Ok(CalculationAdmission {
            state: self,
            _guard: self.admission.enter()?,
        })
    }

    pub(crate) fn seal_admission(&self) {
        self.admission.begin_close();
    }

    /// Seals future admissions immediately. Waiting is a separate transition,
    /// so callers can seal every calculation before waiting for any of them.
    pub(crate) fn close_admission(&self) -> ClosingCalculation<'_> {
        ClosingCalculation {
            state: self,
            wait: self.admission.close_and_wait_begin(),
        }
    }

    pub(crate) fn pin_count(&self) -> usize {
        self.pins.load(Ordering::Acquire)
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn active_admissions(&self) -> usize {
        self.admission.active()
    }

    #[cfg(test)]
    pub(crate) fn tasks_are_empty(&self) -> bool {
        self.shards
            .iter()
            .all(|shard| shard.tasks.lock().is_empty())
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn task_shards_are_unlocked_and_empty(&self) -> bool {
        self.shards
            .iter()
            .all(|shard| shard.tasks.try_lock().is_some_and(|tasks| tasks.is_empty()))
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn task_shard_is_unlocked(&self, id: u64) -> bool {
        self.shards[task_shard(id)].tasks.try_lock().is_some()
    }
}

/// A live admission into exactly one calculation. Only this capability can
/// publish task controls; dropping it releases the close/drain barrier.
#[must_use = "retain calculation admission until task registration finishes"]
pub(crate) struct CalculationAdmission<'a> {
    state: &'a CalculationState,
    _guard: OperationGuard<'a>,
}

impl CalculationAdmission<'_> {
    pub(crate) fn insert_task(&self, id: u64, control: TaskControl) {
        let previous = self.state.shards[task_shard(id)]
            .tasks
            .lock()
            .insert(id, control);
        if previous.is_some() {
            xlfn_kernel::invariant::fail_stop();
        }
    }
}

/// Admission has been sealed, but existing reservations can still insert tasks.
/// Dropping this capability keeps admission sealed and does not wait.
#[must_use = "wait for existing admissions before draining task controls"]
pub(crate) struct ClosingCalculation<'a> {
    state: &'a CalculationState,
    wait: TerminationWaitGuard<'a>,
}

impl<'a> ClosingCalculation<'a> {
    pub(crate) fn wait(self) -> AdmissionDrainedCalculation<'a> {
        self.wait.wait();
        AdmissionDrainedCalculation { state: self.state }
    }
}

/// Every admission has ended; task completions and allocation pins can remain.
/// This capability neither certifies task completion nor permits reclamation.
#[must_use = "use the drained-admission capability to drain task controls"]
pub(crate) struct AdmissionDrainedCalculation<'a> {
    state: &'a CalculationState,
}

impl AdmissionDrainedCalculation<'_> {
    pub(crate) fn drain_tasks(self) -> TaskControlBatch {
        // The consumed capability proves there can be no further insertions,
        // so controls can only disappear while lengths are sampled. This
        // cold scan provides an upper bound without forcing every spawn and
        // completion to update a shared allocation-hint counter.
        let capacity = self
            .state
            .shards
            .iter()
            .map(|shard| shard.tasks.lock().len())
            .sum();
        // Callers drop/wake the controls after releasing the registry control
        // lock. No TaskControl destructor runs while a shard lock is held.
        let mut result = TaskControlBatch::with_capacity(capacity);
        for shard in self.state.shards.iter() {
            let mut tasks = shard.tasks.lock();
            result.extend(tasks.drain().map(|(_, task)| task));
        }
        result
    }
}

/// Non-owning calculation capability. The registry owns the Box and only
/// retires non-current calculations with no pins under its publication lock.
pub(crate) struct CalculationPin(NonNull<CalculationState>);

impl CalculationPin {
    /// # Safety
    /// The caller must hold the registry's calculation-publication lock and
    /// keep the registry alive until this pin is released.
    pub(crate) unsafe fn acquire(state: &CalculationState) -> Self {
        // The publication lock already acquires initialization and excludes
        // reclamation while this lifetime reservation is added.
        state
            .pins
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |pins| {
                pins.checked_add(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        Self(NonNull::from(state))
    }

    pub(crate) fn pointer(&self) -> NonNull<CalculationState> {
        self.0
    }

    pub(crate) fn get(&self) -> &CalculationState {
        // SAFETY: the pin prevents this allocation from being reclaimed.
        unsafe { self.0.as_ref() }
    }
}

impl Drop for CalculationPin {
    fn drop(&mut self) {
        // No calculation access may follow the release: a concurrent advance
        // can immediately reclaim the Box when this was its final pin. Its
        // Acquire zero observation must see every pin holder's prior accesses.
        let _ = xlfn_kernel::invariant::checked_atomic_dec_release(&self.get().pins);
    }
}

// SAFETY: CalculationState is Sync, and the pin follows its user across threads.
unsafe impl Send for CalculationPin {}

#[cfg(feature = "bench-internals")]
/// Benchmark fixture holding task controls in production calculation shards.
pub struct AsyncTaskDrainBenchmark {
    state: CalculationState,
}

#[cfg(feature = "bench-internals")]
impl AsyncTaskDrainBenchmark {
    /// Seeds a calculation with the requested task-control count.
    pub fn new(count: usize) -> Self {
        let state = CalculationState::new(CalculationEpoch::INITIAL);
        let admission = state.enter_admission().expect("new calculation is open");
        for id in 0..count as u64 {
            let (cancellation, _) = crate::cancellation::CancellationSource::new(
                crate::cancellation::CancellationGuarantee::BestEffort,
            );
            admission.insert_task(id, TaskControl { cancellation });
        }
        drop(admission);
        Self { state }
    }

    /// Includes draining and dropping controls; excludes fixture construction.
    pub fn run(&self) -> usize {
        let controls = self.state.close_admission().wait().drain_tasks();
        let count = controls.len();
        std::hint::black_box(controls);
        count
    }

    /// Returns the sizes in bytes of one control and an empty drained collection.
    pub fn sizes() -> (usize, usize) {
        let state = CalculationState::new(CalculationEpoch::INITIAL);
        let controls = state.close_admission().wait().drain_tasks();
        (
            std::mem::size_of::<TaskControl>(),
            std::mem::size_of_val(&controls),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlfn_kernel::published_owner::PublishedOwner;

    #[test]
    fn miri_calculation_close_waits_for_admitted_insert_and_preserves_pins() {
        let owner = PublishedOwner::new(CalculationState::new(CalculationEpoch::INITIAL));
        // SAFETY: this private owner remains alive until after the pin drops.
        let pin = unsafe { CalculationPin::acquire(&owner) };
        let admission = owner.enter_admission().unwrap();
        let closing = owner.close_admission();
        assert!(owner.enter_admission().is_err());
        let (cancellation, token) = crate::cancellation::CancellationSource::new(
            crate::cancellation::CancellationGuarantee::BestEffort,
        );
        let (waiting_tx, waiting_rx) = std::sync::mpsc::sync_channel(1);
        let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let waiter = scope.spawn(move || {
                waiting_tx.send(()).unwrap();
                assert!(drained_tx.send(closing.wait().drain_tasks()).is_ok());
            });
            waiting_rx.recv().unwrap();
            assert!(matches!(
                drained_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
            // An admission acquired before sealing can still publish its task.
            admission.insert_task(1, TaskControl { cancellation });
            drop(admission);
            let controls = drained_rx.recv().unwrap();
            assert_eq!(controls.len(), 1);
            assert!(owner.tasks_are_empty());
            assert!(!token.is_cancelled());
            assert_eq!(owner.pins.load(Ordering::Acquire), 1);
            drop(controls);
            assert!(token.is_cancelled());
            assert_eq!(owner.pins.load(Ordering::Acquire), 1);
            waiter.join().unwrap();
        });
        drop(pin);
        assert_eq!(owner.pins.load(Ordering::Acquire), 0);
    }

    #[test]
    fn miri_calculation_final_pin_release_can_race_owner_reclamation() {
        for id in 0..16 {
            let owner = PublishedOwner::new(CalculationState::new(
                CalculationEpoch::new(id + 1).unwrap(),
            ));
            // SAFETY: the owner is private until both pins are acquired;
            // reclamation below waits for their terminal publication.
            let first = unsafe { CalculationPin::acquire(&owner) };
            // SAFETY: the owner is still private and both pins are drained below.
            let second = unsafe { CalculationPin::acquire(&owner) };
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
