// module: subscription/multiplex
use super::RunningWorker;
fn invalid(running: RunningWorker<'_, fn()>) {
    running.restore(); // error: E0599
}
