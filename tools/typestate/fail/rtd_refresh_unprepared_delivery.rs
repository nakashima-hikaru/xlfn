// module: subscription/data_plane
use super::refresh::RtdRefreshBatch;
use crate::subscription::host::SubscriptionHost;

fn invalid<H: SubscriptionHost>(batch: RtdRefreshBatch<'_, H>) {
    let _ = batch.complete(crate::subscription::delivery::RefreshOutcome::Delivered); // error: E0599
}
