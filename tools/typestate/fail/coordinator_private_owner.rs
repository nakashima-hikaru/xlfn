// module: subscription
use super::termination::{TerminationClaim, TerminationCoordinator};

fn invalid(coordinator: &TerminationCoordinator) {
    if let TerminationClaim::Owner(owner) = coordinator.begin() {
        let _ = &owner.coordinator; // error: E0616
    }
}
