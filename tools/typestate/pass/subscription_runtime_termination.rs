// module: subscription/runtime
use super::SubscriptionRuntime;
use super::termination::{RuntimeTermination, RuntimeTerminationAdmission};
use crate::subscription::host::SubscriptionHost;
use crate::subscription::source::RtdSubscription;
fn ordered<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    runtime: &SubscriptionRuntime<H, E, C>,
) -> crate::XllResult<()> {
    match RuntimeTermination::begin(runtime) {
        RuntimeTerminationAdmission::Owner(owner) => owner.request_stop().drain_sink_users().join_workers()?.finish(),
        RuntimeTerminationAdmission::Waiter(waiter) => waiter.wait(),
        RuntimeTerminationAdmission::Complete(completed) => completed.result(),
    }
}
