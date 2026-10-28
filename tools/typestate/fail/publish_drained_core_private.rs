// module: subscription
use super::data_plane::{DrainedPublish, PublishCore};
use super::host::SubscriptionHost;

fn invalid<'a, H: SubscriptionHost>(mut drained: DrainedPublish<'a, H>, other: &'a PublishCore<H>) {
    drained.publish = other; // error: E0616
}
