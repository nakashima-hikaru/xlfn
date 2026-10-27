// module: subscription
use super::host::SubscriptionHost;
use super::runtime::SubscriptionConnection;

fn invalid<H: SubscriptionHost>(connection: SubscriptionConnection<H>) -> crate::XllResult<()> {
    connection.commit()?;
    connection.rollback(); // error: E0382
    Ok(())
}
