// module: subscription
use super::catalog::SubscriptionEntry;

fn invalid(entry: &SubscriptionEntry) {
    let _ = &entry.state; // error: E0616
}
