// module: subscription/data_plane
use super::{PublishCore, NotificationAttempt};
use crate::subscription::host::SubscriptionHost;
fn invalid<'a, H: SubscriptionHost>(mut attempt: NotificationAttempt<'a, H>, other: &'a PublishCore<H>) {
    attempt.publish = other; // error: E0616
    let _ = attempt.attempt; // error: E0616
}
