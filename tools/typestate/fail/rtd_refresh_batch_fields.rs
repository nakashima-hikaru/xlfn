// module: subscription/data_plane
use super::refresh::RtdRefreshBatch;
use crate::subscription::host::SubscriptionHost;
fn invalid<H: SubscriptionHost>(mut batch: RtdRefreshBatch<'_, H>) {
    batch.plan.refresh_id = 1; // error: E0616
    let _ = batch._operation.take(); // error: E0616
    let _ = std::mem::take(&mut batch.updates); // error: E0616
    batch.finished = true; // error: E0616
}
