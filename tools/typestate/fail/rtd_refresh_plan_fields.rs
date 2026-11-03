// module: subscription/data_plane
use super::{PublishCore, refresh::PlannedRtdRefresh};
use crate::subscription::host::SubscriptionHost;
fn invalid<'a, H: SubscriptionHost>(mut plan: PlannedRtdRefresh<'a, H>, other: &'a PublishCore<H>) {
    plan.publish = other; // error: E0616
    let _ = plan.operation.take(); // error: E0616
    plan.plan.refresh_id = 1; // error: E0616
    plan.updates.clear(); // error: E0616
    plan.finished = true; // error: E0616
}
