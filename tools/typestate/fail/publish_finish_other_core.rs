// module: subscription
use super::data_plane::{DrainedPublish, PublishCore};
use super::host::SubscriptionHost;

fn invalid<H: SubscriptionHost>(drained: DrainedPublish<'_, H>, other: &PublishCore<H>) {
    let _ = drained.finish(other); // error: E0061
}
