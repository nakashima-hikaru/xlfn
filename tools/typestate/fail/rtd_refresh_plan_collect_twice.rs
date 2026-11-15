// module: subscription/data_plane
use super::refresh::PlannedRtdRefresh;
use crate::subscription::host::SubscriptionHost;

fn collect_twice<H: SubscriptionHost>(plan: PlannedRtdRefresh<'_, H>) {
    drop(plan.collect());
    drop(plan.collect()); // error: E0382
}
