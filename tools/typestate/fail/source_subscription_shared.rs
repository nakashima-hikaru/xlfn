// module: subscription
use super::source::SourceSubscription;

fn require_sync<T: Sync>() {}

fn invalid() {
    require_sync::<SourceSubscription>(); // error: E0277
}
