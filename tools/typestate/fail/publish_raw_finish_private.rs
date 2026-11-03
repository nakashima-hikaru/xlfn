// module: subscription
use super::data_plane::PublishCore;
use super::host::SubscriptionHost;

fn invalid<H: SubscriptionHost>(publish: &PublishCore<H>) {
    let _ = publish.finish_termination(); // error: E0624
}
