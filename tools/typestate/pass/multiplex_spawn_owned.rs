// module: subscription/multiplex
use super::{WorkerStart, WorkerPermit};
fn valid(start: WorkerStart<'_, fn()>, permit: WorkerPermit) {
    if let Ok(running) = start.spawn(permit, |job| job(), false) {
        running.install();
    }
}
