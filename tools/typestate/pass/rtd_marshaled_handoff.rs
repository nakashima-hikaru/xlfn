// module: subscription/data_plane/refresh/windows
use super::{RtdRefreshBatch, SubscriptionHost, RefreshOutputSlots};
fn ordered<H: SubscriptionHost>(batch: RtdRefreshBatch<'_, H>, output: RefreshOutputSlots<'_>) -> Result<crate::XllResult<()>, i32> {
    let prepared = batch.marshal()?;
    Ok(prepared.handoff(output)?.completion())
}
