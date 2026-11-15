// module: subscription
use super::PublishCore;
use super::host::SubscriptionHost;
use crate::XllResult;

fn release<H: SubscriptionHost>(core: &PublishCore<H>) -> XllResult<()> {
    let admission = core.enter_operation()?;
    drop(admission);
    Ok(())
}
