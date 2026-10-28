// module: subscription
use super::data_plane::DrainedPublish;
use super::host::SubscriptionHost;

fn invalid<H: SubscriptionHost>(drained: DrainedPublish<'_, H>) {
    let _ = drained.finish();
    let _ = drained.finish(); // error: E0382
}
