// module: subscription/runtime
use super::{PreparedSubscription, RtdSubscription, SubscriptionHost};
use crate::RtdSourceCatalog;

fn resolve_twice<H: SubscriptionHost, E: RtdSubscription, C: RtdSourceCatalog<E>>(
    prepared: PreparedSubscription<'_, H, E, C>,
) {
    prepared.commit();
    prepared.rollback(); // error: E0382
}
