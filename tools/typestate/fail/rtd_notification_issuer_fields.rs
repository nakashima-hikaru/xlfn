// module: subscription/data_plane
use super::NotificationAttempt;
use crate::subscription::host::SubscriptionHost;
fn invalid<'a, H: SubscriptionHost>(mut attempt: NotificationAttempt<'a, H>, other: NotificationAttempt<'a, H>) {
    attempt.pending = other.pending; // error: E0616
    let _ = attempt.attempt; // error: E0616
}
