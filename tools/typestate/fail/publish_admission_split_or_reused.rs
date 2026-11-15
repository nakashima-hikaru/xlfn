// module: subscription
use super::data_plane::{OwnedPublishOperation, ScopedPublishOperation};
use super::host::SubscriptionHost;

fn split<H: SubscriptionHost>(operation: ScopedPublishOperation<'_, H>) {
    drop(operation._admissions); // error: E0616
}

fn release_twice<H: SubscriptionHost>(operation: OwnedPublishOperation<H>) {
    drop(operation);
    drop(operation); // error: E0382
}
