// module: subscription
use super::host::SubscriptionHost;
use super::runtime::SubscriptionConnection;

fn commit<H: SubscriptionHost>(connection: SubscriptionConnection<H>) -> crate::XllResult<()> {
    connection.commit()
}

fn rollback<H: SubscriptionHost>(connection: SubscriptionConnection<H>) {
    connection.rollback();
}
