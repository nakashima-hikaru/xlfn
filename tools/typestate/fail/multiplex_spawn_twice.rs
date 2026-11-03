// module: subscription/multiplex
use super::{WorkerStart, WorkerPermit};
fn invalid(start: WorkerStart<'_, fn()>, first: WorkerPermit, second: WorkerPermit) {
    let _ = start.spawn(first, |job| job(), false);
    let _ = start.spawn(second, |job| job(), false); // error: E0382
}
