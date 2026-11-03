// module: subscription
use super::data_plane::PublishTerminationStart;
use super::host::SubscriptionHost;

fn invalid<H: SubscriptionHost>(start: PublishTerminationStart<'_, H>) {
    let _ = start.finish(); // error: E0599
}
