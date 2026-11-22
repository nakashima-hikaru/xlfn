// module: subscription
use super::termination::{TerminationClaim, TerminationCoordinator};

fn complete_twice(coordinator: &TerminationCoordinator) {
    if let TerminationClaim::Owner(owner) = coordinator.begin() {
        let _ = owner.complete(Ok(()));
        let _ = owner.complete(Ok(())); // error: E0382
    }
}

fn fail_after_complete(coordinator: &TerminationCoordinator) {
    if let TerminationClaim::Owner(owner) = coordinator.begin() {
        let _ = owner.complete(Ok(()));
        let _ = owner.fail(crate::XllError::Panic); // error: E0382
    }
}
