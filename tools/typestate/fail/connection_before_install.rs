// module: subscription/runtime
use super::{CatalogReserved, ConnectionTransaction};
use crate::subscription::host::SubscriptionHost;

fn invalid<H: SubscriptionHost>(connection: ConnectionTransaction<H, CatalogReserved>) {
    connection.commit(); // error: E0599
}
