// module: async_udf
use super::calculation::AdmissionDrainedCalculation;

fn invalid(drained: AdmissionDrainedCalculation<'_>) {
    drop(drained.drain_tasks());
    drop(drained.drain_tasks()); // error: E0382
}
