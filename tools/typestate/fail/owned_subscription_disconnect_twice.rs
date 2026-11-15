// module: subscription/disconnect
use super::{OwnedSubscription, RtdSubscription, SubscriptionHost};
use crate::RtdSourceCatalog;

fn disconnect_twice<H: SubscriptionHost, E: RtdSubscription, C: RtdSourceCatalog<E>>(
    owner: OwnedSubscription<H, E, C>,
) {
    let _ = owner.disconnect_and_wait();
    let _ = owner.disconnect_and_wait(); // error: E0382
}
