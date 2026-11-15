// module: subscription/channel
use super::RunningPublisher;

fn invalid(running: RunningPublisher) {
    running.join();
    running.join(); // error: E0382
}
