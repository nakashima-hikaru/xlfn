// module: subscription/data_plane
use super::NotificationAttempt;
use crate::subscription::host::SubscriptionHost;
fn invalid<H: SubscriptionHost>(attempt: NotificationAttempt<'_, H>) {
    attempt.drive();
    attempt.drive(); // error: E0382
}
