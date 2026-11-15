// module: subscription/channel
use super::{JoinedPublisher, RunningPublisher};

fn join(running: RunningPublisher) -> JoinedPublisher {
    running.join()
}
