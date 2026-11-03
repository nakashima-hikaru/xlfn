// module: subscription
use super::termination::{TerminationClaim, TerminationCoordinator};

fn invalid(coordinator: &TerminationCoordinator) {
    if let TerminationClaim::Waiter(waiter) = coordinator.begin() {
        let _ = waiter.wait();
        let _ = waiter.wait(); // error: E0382
    }
}
