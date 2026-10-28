// module: subscription
use super::data_plane::PublishTerminationStart;
use super::host::SubscriptionHost;

fn finish<H: SubscriptionHost>(start: PublishTerminationStart<'_, H>) {
    let _ = start.wait().finish();
}
