// module: subscription
use super::termination::TerminationCoordinator;

fn invalid(coordinator: &TerminationCoordinator) {
    let _ = &coordinator.state; // error: E0616
}
