// module: subscription/multiplex
use super::{UnstartedWorker, WorkerPermit, WorkerStart};
fn prepare(unstarted: UnstartedWorker<'_, fn()>, permit: WorkerPermit) {
    valid(unstarted.capture(), permit);
}
fn valid(start: WorkerStart<'_, fn()>, permit: WorkerPermit) {
    if let Ok(running) = start.spawn(permit, |job| job(), false) {
        running.install();
    }
}
