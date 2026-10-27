// module: subscription
use super::termination::{TerminationClaim, TerminationCoordinator};

fn invalid(coordinator: &TerminationCoordinator) {
    if let TerminationClaim::Owner(owner) = coordinator.begin() {
        let _ = owner.complete(Ok(()));
        let _ = owner.fail(crate::XllError::Panic); // error: E0382
    }
}
