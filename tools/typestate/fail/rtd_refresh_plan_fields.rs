// module: subscription/data_plane
use super::refresh::PlannedRtdRefresh;
use crate::subscription::host::SubscriptionHost;
fn invalid<H: SubscriptionHost>(plan: PlannedRtdRefresh<'_, H>) {
    let _ = plan.pending.complete(); // error: E0616
}
