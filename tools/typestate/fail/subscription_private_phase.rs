// module: subscription
use super::catalog::SubscriptionEntry;

fn invalid(entry: &SubscriptionEntry) {
    let _ = &entry.phase; // error: E0616
}
