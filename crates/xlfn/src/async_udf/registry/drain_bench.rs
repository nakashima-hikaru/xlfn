use super::*;

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
            let (cancellation, _) = crate::cancellation::TaskCancellationOwner::new(
                crate::cancellation::CancellationGuarantee::BestEffort,
            );
            admission.insert_task(
                TaskRegistrationId(id),
                TaskControl {
                    cancellation: cancellation.into_parts().0,
                },
            );
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
