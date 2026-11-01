// module: subscription/data_plane
use super::PublishCore;
use crate::subscription::host::SubscriptionHost;
fn ordered<H: SubscriptionHost>(publish: &PublishCore<H>) -> crate::XllResult<()> {
    let planned = publish.plan_refresh()?;
    let batch = planned.collect();
    let _values = batch.updates();
    drop(batch);
    Ok(())
}
