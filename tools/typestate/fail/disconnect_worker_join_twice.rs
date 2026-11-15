// module: subscription/disconnect
use super::{DrainWork, RtdSubscription, RunningDisconnect, SubscriptionHost};
use crate::RtdSourceCatalog;

fn invalid<H: SubscriptionHost, E: RtdSubscription, C: RtdSourceCatalog<E>>(
    worker: RunningDisconnect<H, E, C>,
) {
    worker.run();
    worker.run(); // error: E0382
}
