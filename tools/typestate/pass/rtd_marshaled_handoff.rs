// module: subscription/data_plane/refresh/windows
use super::{RtdRefreshBatch, SubscriptionHost, RefreshOutputSlots};
fn ordered<H: SubscriptionHost>(batch: RtdRefreshBatch<'_, H>, output: RefreshOutputSlots<'_>) -> Result<crate::XllResult<()>, i32> {
    let prepared = batch.marshal()?;
    Ok(prepared.handoff(output)?.completion())
}

fn inspect_host_array(
    updates: &[super::RtdUpdate],
) -> Result<i32, xlfn_host::refresh_array::MarshalFailure> {
    let array = xlfn_host::refresh_array::ReadyArray::<crate::excel_rtd::WindowsRefreshArrayApi>::prepare(updates)?;
    Ok(array.count())
}
