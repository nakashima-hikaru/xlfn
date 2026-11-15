// module: subscription/multiplex
use super::{RunningWorker, SpawningWorker, UnstartedWorker};
fn capture_twice(unstarted: UnstartedWorker<'_, fn()>) {
    drop(unstarted.capture());
    drop(unstarted.capture()); // error: E0382
}
fn install_twice(running: RunningWorker<'_, fn()>) {
    running.install();
    running.install(); // error: E0382
}
fn restore_without_outcome(spawning: SpawningWorker<'_, fn()>) {
    spawning.restore(); // error: E0599
}
