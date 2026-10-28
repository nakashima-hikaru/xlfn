// module: subscription
use super::host::SubscriptionHost;
use super::server::{CancelPending, ServerTermination};
use super::termination::{TerminationClaim, TerminationCoordinator};

fn terminate<H: SubscriptionHost>(
    pending: ServerTermination<'_, H, (), crate::DynamicRtdSources<()>, CancelPending>,
) -> crate::XllResult<()> {
    pending.request_cancel().finish()
}

fn coordinate(coordinator: &TerminationCoordinator) -> crate::XllResult<()> {
    match coordinator.begin() {
        TerminationClaim::Owner(owner) => owner.complete(Ok(())),
        TerminationClaim::Waiter(waiter) => waiter.wait(),
        TerminationClaim::Complete(completed) => completed.result(),
    }
}

fn fail(coordinator: &TerminationCoordinator) -> crate::XllResult<()> {
    match coordinator.begin() {
        TerminationClaim::Owner(owner) => owner.fail(crate::XllError::Panic),
        TerminationClaim::Waiter(waiter) => waiter.wait(),
        TerminationClaim::Complete(completed) => completed.result(),
    }
}
