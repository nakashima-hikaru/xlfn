// module: subscription/data_plane/refresh/windows
use super::{RtdRefreshBatch, SubscriptionHost, RefreshOutputSlots};
fn invalid<H: SubscriptionHost>(batch: RtdRefreshBatch<'_, H>, first: RefreshOutputSlots<'_>, second: RefreshOutputSlots<'_>) {
    let prepared = batch.marshal().unwrap();
    let _ = prepared.handoff(first);
    let _ = prepared.handoff(second); // error: E0382
}
